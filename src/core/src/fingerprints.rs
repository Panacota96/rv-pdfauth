//! Producer/editor fingerprint databases handed to the PDF layer, and their identity for the audit trail.
//!
//! Three sources, in this order:
//! 1. **starter** — bundled with the PDF layer (`rv_pdfauth_pdf::starter_db()`, behind the `starter-db`
//!    feature until that function lands; without it an empty DB is recorded as `unavailable`);
//! 2. **local** — the examiner's learned DB: `%LOCALAPPDATA%\Rakvor\rv-pdfauth\fingerprints.local.json`
//!    (Windows) or `$XDG_DATA_HOME/rakvor/rv-pdfauth/fingerprints.local.json`, default
//!    `~/.local/share/rakvor/rv-pdfauth/` (Linux/macOS);
//! 3. **extra** — `DomainOptions::fingerprints_db` (CLI/GUI option).
//!
//! Each is described by role, name, version, entry count and SHA-256 (of the file bytes, or of the canonical
//! JSON for the bundled DB) in `details.fingerprint_dbs`, which the pipeline writes into `item_analyzed`:
//! a second examiner can tell exactly which knowledge produced a fingerprint match.

use crate::hash::{HashAlgo, hash_bytes};
use rv_pdfauth_pdf::FingerprintDb;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct LoadedDbs {
    /// DBs passed to the parser (only the ones that loaded).
    pub dbs: Vec<FingerprintDb>,
    /// One entry per source, loaded or not (for `details.fingerprint_dbs`).
    pub details: Vec<Value>,
}

/// Bundled starter DB (adapter: compiles before and after the PDF layer exposes `starter_db()`).
#[cfg(feature = "starter-db")]
fn starter_db() -> Option<FingerprintDb> {
    Some(rv_pdfauth_pdf::starter_db())
}

#[cfg(not(feature = "starter-db"))]
fn starter_db() -> Option<FingerprintDb> {
    None
}

/// Default location of the examiner's learned DB on this OS (None if no home/app-data folder is known).
pub fn local_db_path() -> Option<PathBuf> {
    local_db_path_from(|k| std::env::var_os(k))
}

fn local_db_path_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let nonempty = |k: &str| var(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        nonempty("LOCALAPPDATA").map(|p| p.join("Rakvor").join("rv-pdfauth"))
    } else {
        nonempty("XDG_DATA_HOME")
            .or_else(|| nonempty("HOME").map(|h| h.join(".local").join("share")))
            .map(|p| p.join("rakvor").join("rv-pdfauth"))
    }?;
    Some(base.join("fingerprints.local.json"))
}

fn canonical_sha256(db: &FingerprintDb) -> String {
    // serde_json::Value maps are sorted: the digest does not depend on struct field order.
    let v = serde_json::to_value(db).unwrap_or(Value::Null);
    hash_bytes(
        serde_json::to_string(&v).unwrap_or_default().as_bytes(),
        HashAlgo::Sha256,
    )
}

fn from_file(role: &str, path: &Path, missing_ok: bool) -> (Option<FingerprintDb>, Value) {
    let p = path.display().to_string();
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && missing_ok => {
            (None, json!({"role": role, "path": p, "status": "absent"}))
        }
        Err(e) => (
            None,
            json!({"role": role, "path": p, "status": "error", "error": e.to_string()}),
        ),
        Ok(bytes) => {
            let sha256 = hash_bytes(&bytes, HashAlgo::Sha256);
            match serde_json::from_slice::<FingerprintDb>(&bytes) {
                Ok(db) => {
                    let d = json!({"role": role, "path": p, "status": "loaded", "name": db.name,
                        "version": db.version, "entries": db.entries.len(), "sha256": sha256, "bytes": bytes.len()});
                    (Some(db), d)
                }
                Err(e) => (
                    None,
                    json!({"role": role, "path": p, "status": "error", "sha256": sha256,
                        "error": format!("not a fingerprint database: {}", e)}),
                ),
            }
        }
    }
}

/// Loads every source. Never fails: problems are described in `details` and the source is skipped.
pub fn load(extra: Option<&Path>) -> LoadedDbs {
    load_with(starter_db(), local_db_path(), extra)
}

fn load_with(
    starter: Option<FingerprintDb>,
    local: Option<PathBuf>,
    extra: Option<&Path>,
) -> LoadedDbs {
    let mut out = LoadedDbs {
        dbs: vec![],
        details: vec![],
    };
    match starter {
        Some(db) => {
            out.details.push(json!({"role": "starter", "status": "loaded", "name": db.name,
                "version": db.version, "entries": db.entries.len(), "sha256": canonical_sha256(&db)}));
            out.dbs.push(db);
        }
        None => out
            .details
            .push(json!({"role": "starter", "status": "unavailable",
            "reason": "the PDF layer of this build has no bundled fingerprint database"})),
    }
    match local {
        Some(p) => {
            let (db, d) = from_file("local", &p, true);
            out.details.push(d);
            out.dbs.extend(db);
        }
        None => out.details.push(json!({"role": "local", "status": "absent",
            "reason": "no LOCALAPPDATA / XDG_DATA_HOME / HOME"})),
    }
    if let Some(p) = extra {
        let (db, d) = from_file("extra", p, false);
        out.details.push(d);
        out.dbs.extend(db);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn local_path_follows_the_platform_convention() {
        let env = |k: &str| -> Option<OsString> {
            match k {
                "LOCALAPPDATA" => Some("C:\\Users\\ex\\AppData\\Local".into()),
                "HOME" => Some("/home/ex".into()),
                _ => None,
            }
        };
        let p = local_db_path_from(env).unwrap();
        if cfg!(windows) {
            assert!(p.ends_with("Rakvor/rv-pdfauth/fingerprints.local.json"));
        } else {
            assert_eq!(
                p,
                Path::new("/home/ex/.local/share/rakvor/rv-pdfauth/fingerprints.local.json")
            );
            let xdg = |k: &str| (k == "XDG_DATA_HOME").then(|| OsString::from("/data"));
            assert_eq!(
                local_db_path_from(xdg).unwrap(),
                Path::new("/data/rakvor/rv-pdfauth/fingerprints.local.json")
            );
        }
    }

    #[test]
    fn sources_are_described_with_digests() {
        let dir = tempfile::tempdir().unwrap();
        let extra = dir.path().join("extra.json");
        std::fs::write(
            &extra,
            r#"{"name":"case-42","version":"3","entries":[{"id":"x","label":"X","category":"pdf_editor"}]}"#,
        )
        .unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        let starter = FingerprintDb {
            name: "starter".into(),
            version: "2026.10".into(),
            entries: vec![],
        };
        let r = load_with(
            Some(starter),
            Some(dir.path().join("missing.json")),
            Some(&extra),
        );
        assert_eq!(r.dbs.len(), 2);
        assert_eq!(r.details[0]["status"], "loaded");
        assert_eq!(r.details[0]["sha256"].as_str().unwrap().len(), 64);
        assert_eq!(r.details[1]["status"], "absent");
        assert_eq!(r.details[2]["name"], "case-42");
        assert_eq!(r.details[2]["entries"], 1);
        assert_eq!(
            r.details[2]["sha256"],
            hash_bytes(&std::fs::read(&extra).unwrap(), HashAlgo::Sha256)
        );
        let r = load_with(None, None, Some(&bad));
        assert!(r.dbs.is_empty());
        assert_eq!(r.details[0]["status"], "unavailable");
        assert_eq!(r.details[2]["status"], "error");
    }
}
