//! `rv-pdfauth fingerprints learn|list|export`: the examiner's local producer/editor database.
//!
//! Learning reads known-original PDFs read-only, takes their Producer/Creator strings and the structural
//! traits the PDF layer observes for the revision those strings come from, and adds (or merges) one entry
//! in the local DB (`rv_pdfauth_core::fingerprints::local_db_path`). Nothing is sent anywhere. Every change
//! is appended to `fingerprints.audit.jsonl` next to the DB (hash-chained, with the DB digest before and
//! after), and the digests are printed.

use rv_pdfauth_core::hash::{HashAlgo, hash_bytes};
use rv_pdfauth_core::{TOOL_NAME, TOOL_VERSION};
use rv_pdfauth_pdf::{FingerprintDb, FingerprintEntry, PdfOptions, PdfReport};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const CATEGORIES: [&str; 8] = [
    "office",
    "pdf_library",
    "scanner",
    "pdf_editor",
    "online_editor",
    "unlocker",
    "printer_driver",
    "browser",
];

pub const LOCAL_DB_NAME: &str = "rakvor-local";
const AUDIT_FILE: &str = "fingerprints.audit.jsonl";
/// Traits learned from a known original (header_version is left out: one tool writes several versions).
const LEARNED_TRAITS: [&str; 4] = [
    "xref_kind",
    "object_streams",
    "binary_comment_hex",
    "header_comment_contains",
];
const PROBE: &str = "\u{1}rv-probe";

fn sha256(b: &[u8]) -> String {
    hash_bytes(b, HashAlgo::Sha256)
}

pub fn local_db() -> Result<PathBuf, String> {
    rv_pdfauth_core::fingerprints::local_db_path().ok_or_else(|| {
        "no folder for the local fingerprint database (set LOCALAPPDATA, XDG_DATA_HOME or HOME)"
            .into()
    })
}

/// (bytes, parsed DB) of the local DB; Ok(None) when it does not exist yet.
fn read_local(path: &Path) -> Result<Option<(Vec<u8>, FingerprintDb)>, String> {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
        Ok(b) => serde_json::from_slice::<FingerprintDb>(&b)
            .map(|db| Some((b, db)))
            .map_err(|e| {
                format!(
                    "{} is not a fingerprint database ({}); it was left unchanged",
                    path.display(),
                    e
                )
            }),
    }
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
}

/// What one known-original file teaches: its strings and the traits of the revision they come from.
struct Learned {
    path: PathBuf,
    bytes: u64,
    sha256: String,
    producer: Vec<String>,
    creator: Vec<String>,
    traits: BTreeMap<String, String>,
}

fn strings_of(report: &PdfReport) -> Option<(usize, Vec<String>, Vec<String>)> {
    for r in &report.revisions {
        let mut p: Vec<String> = [
            nonempty(&r.info.producer),
            r.xmp.as_ref().and_then(|x| nonempty(&x.producer)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut c: Vec<String> = [
            nonempty(&r.info.creator),
            r.xmp.as_ref().and_then(|x| nonempty(&x.creator_tool)),
        ]
        .into_iter()
        .flatten()
        .collect();
        p.dedup();
        c.dedup();
        if !p.is_empty() || !c.is_empty() {
            return Some((r.index, p, c));
        }
    }
    None
}

/// Observed structural traits of revision `rev`, read through the PDF layer's own trait comparison: a probe
/// entry that expects impossible values makes the parser report what it found for every trait.
fn observed_traits(
    bytes: &[u8],
    rev: usize,
    producer: &[String],
    creator: &[String],
) -> BTreeMap<String, String> {
    let probe = FingerprintDb {
        name: "__rv_probe".into(),
        version: "0".into(),
        entries: vec![FingerprintEntry {
            id: "__probe".into(),
            label: "probe".into(),
            category: "probe".into(),
            producer_contains: producer.to_vec(),
            creator_contains: creator.to_vec(),
            traits: LEARNED_TRAITS
                .iter()
                .map(|k| (k.to_string(), PROBE.to_string()))
                .collect(),
            source: "probe".into(),
        }],
    };
    let rep = rv_pdfauth_pdf::analyze(
        bytes,
        &PdfOptions {
            fingerprints: vec![probe],
            ..PdfOptions::default()
        },
    );
    let mut out = BTreeMap::new();
    for m in rep
        .fingerprints
        .iter()
        .filter(|m| m.db == "__rv_probe" && m.revision == rev)
    {
        for t in &m.trait_mismatches {
            let Some((k, rest)) = t.split_once(": expected ") else {
                continue;
            };
            let Some(found) = rest
                .strip_prefix(PROBE)
                .and_then(|r| r.strip_prefix(", found "))
            else {
                continue;
            };
            if found != "(none)" && !found.trim().is_empty() {
                out.insert(k.to_string(), found.to_string());
            }
        }
    }
    out
}

fn learn_one(path: &Path) -> Result<Learned, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let report = rv_pdfauth_pdf::analyze(&bytes, &PdfOptions::default());
    if !report.is_pdf {
        return Err(format!("{} is not a PDF", path.display()));
    }
    let (rev, producer, creator) = strings_of(&report).ok_or_else(|| {
        format!(
            "{} has no Producer, Creator or XMP CreatorTool string to learn from",
            path.display()
        )
    })?;
    let traits = observed_traits(&bytes, rev, &producer, &creator);
    Ok(Learned {
        path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        bytes: bytes.len() as u64,
        sha256: sha256(&bytes),
        producer,
        creator,
        traits,
    })
}

fn union_ci(into: &mut Vec<String>, add: &[String]) {
    for a in add {
        if !into.iter().any(|x| x.eq_ignore_ascii_case(a)) {
            into.push(a.clone());
        }
    }
}

/// Keeps only the traits on which every source agrees.
fn intersect(
    a: &BTreeMap<String, String>,
    b: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    a.iter()
        .filter(|(k, v)| b.get(*k) == Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "entry".into() } else { out }
}

/// Appends one hash-chained line to the audit log next to the DB; returns its path.
fn audit(dir: &Path, mut entry: Value) -> Result<PathBuf, String> {
    let path = dir.join(AUDIT_FILE);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let last = existing.lines().rev().find(|l| !l.trim().is_empty());
    let (seq, prev) = match last.and_then(|l| serde_json::from_str::<Value>(l).ok()) {
        Some(v) => (
            v["seq"].as_u64().unwrap_or(0) + 1,
            v["entry_hash"].as_str().unwrap_or_default().to_string(),
        ),
        None => (1, String::new()),
    };
    entry["seq"] = json!(seq);
    entry["prev_entry_hash"] = json!(prev);
    // serde_json maps are sorted: the canonical form is stable.
    let canon = serde_json::to_string(&entry).unwrap_or_default();
    entry["entry_hash"] = json!(sha256(canon.as_bytes()));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {}", path.display(), e))?;
    writeln!(f, "{}", serde_json::to_string(&entry).unwrap_or_default())
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
    Ok(path)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))
}

pub struct LearnRequest<'a> {
    pub files: &'a [PathBuf],
    pub label: &'a str,
    pub category: &'a str,
    pub case_id: Option<&'a str>,
    pub actor: &'a str,
}

/// Returns the text to print. Nothing is written unless every file was learned.
pub fn learn(req: &LearnRequest) -> Result<String, String> {
    let label = req.label.trim();
    if label.is_empty() {
        return Err("--label must not be empty".into());
    }
    let db_path = local_db()?;
    let learned: Vec<Learned> = req
        .files
        .iter()
        .map(|p| learn_one(p))
        .collect::<Result<_, _>>()?;
    let before = read_local(&db_path)?;
    let before_sha = before
        .as_ref()
        .map(|(b, _)| sha256(b))
        .unwrap_or_else(|| "absent".into());
    let mut db = before.map(|(_, db)| db).unwrap_or_else(|| FingerprintDb {
        name: LOCAL_DB_NAME.into(),
        version: "0".into(),
        entries: vec![],
    });

    let mut producer = Vec::new();
    let mut creator = Vec::new();
    let mut traits = learned[0].traits.clone();
    for l in &learned {
        union_ci(&mut producer, &l.producer);
        union_ci(&mut creator, &l.creator);
        traits = intersect(&traits, &l.traits);
    }
    let today = rv_pdfauth_core::time::utc_now();
    let source = format!(
        "learned from case {} on {} by {} from {} known-original file(s): {}",
        req.case_id.unwrap_or("(none)"),
        &today[..10],
        req.actor,
        learned.len(),
        learned
            .iter()
            .map(|l| format!(
                "{} sha256 {}",
                l.path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default(),
                l.sha256
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let id = format!("local-{}", slug(label));
    let merged = if let Some(e) = db.entries.iter_mut().find(|e| e.id == id) {
        union_ci(&mut e.producer_contains, &producer);
        union_ci(&mut e.creator_contains, &creator);
        e.traits = intersect(&e.traits, &traits);
        e.label = label.to_string();
        e.category = req.category.to_string();
        e.source = format!("{}; {}", e.source, source);
        true
    } else {
        db.entries.push(FingerprintEntry {
            id: id.clone(),
            label: label.to_string(),
            category: req.category.to_string(),
            producer_contains: producer.clone(),
            creator_contains: creator.clone(),
            traits: traits.clone(),
            source,
        });
        false
    };
    db.version = (db.version.parse::<u64>().unwrap_or(0) + 1).to_string();
    let entry = db
        .entries
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .unwrap_or_default();

    let dir = db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let mut data = serde_json::to_vec_pretty(&db).map_err(|e| e.to_string())?;
    data.push(b'\n');
    write_atomic(&db_path, &data)?;
    let after_sha = sha256(&data);
    let audit_path = audit(
        &dir,
        json!({
            "time_utc": today, "action": "fingerprints_learn", "tool": TOOL_NAME, "tool_version": TOOL_VERSION,
            "actor": req.actor, "case_id": req.case_id, "entry_id": id, "merged": merged,
            "label": label, "category": req.category,
            "files": learned.iter().map(|l| json!({"path": l.path.display().to_string(), "bytes": l.bytes, "sha256": l.sha256})).collect::<Vec<_>>(),
            "producer_contains": entry.producer_contains, "creator_contains": entry.creator_contains, "traits": entry.traits,
            "db_path": db_path.display().to_string(), "db_version": db.version,
            "db_sha256_before": before_sha, "db_sha256_after": after_sha,
        }),
    )?;
    let fmt_traits = if entry.traits.is_empty() {
        "(none)".to_string()
    } else {
        entry
            .traits
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(format!(
        "{verb} {id} ({label}, {cat}) from {n} file(s)\n  producer contains: {p}\n  creator contains:  {c}\n  traits:            {t}\n  local DB:          {db} (version {ver}, {entries} entries)\n  sha256 before:     {before}\n  sha256 after:      {after}\n  audit log:         {audit}\n  Nothing was sent anywhere; the evidence files were only read.",
        verb = if merged { "UPDATED" } else { "LEARNED" },
        id = id,
        label = label,
        cat = req.category,
        n = learned.len(),
        p = if entry.producer_contains.is_empty() {
            "(none)".into()
        } else {
            entry.producer_contains.join(" | ")
        },
        c = if entry.creator_contains.is_empty() {
            "(none)".into()
        } else {
            entry.creator_contains.join(" | ")
        },
        t = fmt_traits,
        db = db_path.display(),
        ver = db.version,
        entries = db.entries.len(),
        before = before_sha,
        after = after_sha,
        audit = audit_path.display(),
    ))
}

/// `list --db local|starter|all`.
pub fn list(which: &str) -> Result<String, String> {
    let mut out = String::new();
    if which == "starter" || which == "all" {
        let db = rv_pdfauth_pdf::starter_db();
        let digest = rv_pdfauth_core::fingerprints::load(None)
            .details
            .iter()
            .find(|d| d["role"] == "starter")
            .and_then(|d| d["sha256"].as_str().map(String::from))
            .unwrap_or_default();
        out.push_str(&format!(
            "# starter  {} {}  {} entries  sha256 {}  (bundled)\n",
            db.name,
            db.version,
            db.entries.len(),
            digest
        ));
        push_entries(&mut out, &db);
    }
    if which == "local" || which == "all" {
        let path = local_db()?;
        match read_local(&path)? {
            None => out.push_str(&format!(
                "# local  absent  ({})\n  Add entries with `rv-pdfauth fingerprints learn`.\n",
                path.display()
            )),
            Some((bytes, db)) => {
                out.push_str(&format!(
                    "# local  {} {}  {} entries  sha256 {}  ({})\n",
                    db.name,
                    db.version,
                    db.entries.len(),
                    sha256(&bytes),
                    path.display()
                ));
                push_entries(&mut out, &db);
            }
        }
    }
    Ok(out)
}

fn push_entries(out: &mut String, db: &FingerprintDb) {
    for e in &db.entries {
        let mut strings = e.producer_contains.clone();
        union_ci(&mut strings, &e.creator_contains);
        out.push_str(&format!(
            "  {:<24} {:<15} {}  [{}]\n",
            e.id,
            e.category,
            e.label,
            strings.join(" | ")
        ));
    }
}

/// `export --out FILE [--db local|starter]`: never overwrites an existing file.
pub fn export(which: &str, out: &Path) -> Result<String, String> {
    if out.exists() {
        return Err(format!(
            "{} already exists; choose another --out (exports never overwrite)",
            out.display()
        ));
    }
    let data = if which == "starter" {
        let mut d =
            serde_json::to_vec_pretty(&rv_pdfauth_pdf::starter_db()).map_err(|e| e.to_string())?;
        d.push(b'\n');
        d
    } else {
        let path = local_db()?;
        match read_local(&path)? {
            Some((bytes, _)) => bytes,
            None => {
                return Err(format!(
                    "the local fingerprint database does not exist yet ({})",
                    path.display()
                ));
            }
        }
    };
    std::fs::write(out, &data).map_err(|e| format!("cannot write {}: {}", out.display(), e))?;
    Ok(format!(
        "EXPORTED {} database to {}\n  sha256: {}\n  Use it on another workstation with --fingerprints-db {}",
        which,
        out.display(),
        sha256(&data),
        out.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_trait_intersection() {
        assert_eq!(slug("Smallpdf (online)"), "smallpdf-online");
        assert_eq!(slug("  "), "entry");
        let a: BTreeMap<String, String> = [
            ("x".to_string(), "1".to_string()),
            ("y".to_string(), "2".to_string()),
        ]
        .into();
        let b: BTreeMap<String, String> = [
            ("x".to_string(), "1".to_string()),
            ("y".to_string(), "3".to_string()),
        ]
        .into();
        assert_eq!(intersect(&a, &b).len(), 1);
    }
}
