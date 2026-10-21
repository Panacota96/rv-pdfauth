//! `analyze` (renamed from `scan`), its PDF domain options, the new report sections and the
//! `fingerprints learn|list|export` commands.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

/// Offline, and isolated from the developer's own learned fingerprint DB.
fn bin(data: &Path) -> Command {
    let mut c = Command::cargo_bin("rv-pdfauth").unwrap();
    c.env("RV_NO_TIMESTAMP", "1")
        .env("XDG_DATA_HOME", data)
        .env("LOCALAPPDATA", data);
    c
}

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sample/evidence")
}

fn only_dir(p: &Path) -> PathBuf {
    std::fs::read_dir(p)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

fn json(p: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn sha256(b: &[u8]) -> String {
    rv_pdfauth_core::hash::hash_bytes(b, rv_pdfauth_core::HashAlgo::Sha256)
}

fn local_db(data: &Path) -> PathBuf {
    if cfg!(windows) {
        data.join("Rakvor")
            .join("rv-pdfauth")
            .join("fingerprints.local.json")
    } else {
        data.join("rakvor")
            .join("rv-pdfauth")
            .join("fingerprints.local.json")
    }
}

// ---------------------------------------------------------------- rename and help

#[test]
fn scan_is_a_hidden_alias_of_analyze() {
    let d = tempfile::tempdir().unwrap();
    let root = bin(d.path()).arg("--help").output().unwrap();
    let text = String::from_utf8(root.stdout).unwrap();
    assert!(text.contains("analyze"));
    assert!(
        !text.lines().any(|l| l.trim_start().starts_with("scan")),
        "scan must not be listed"
    );
    let alias = bin(d.path()).args(["scan", "--help"]).output().unwrap();
    assert!(alias.status.success());
    assert!(
        String::from_utf8(alias.stdout)
            .unwrap()
            .contains("rv-pdfauth analyze")
    );
}

#[test]
fn help_documents_every_domain_option_within_100_columns() {
    let d = tempfile::tempdir().unwrap();
    let out = bin(d.path()).args(["analyze", "--help"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    for flag in [
        "--revisions",
        "--visual-diff",
        "--dpi",
        "--max-pages",
        "--objects-dump",
        "--extract-embedded",
        "--worker-timeout",
        "--worker-memory",
        "--fingerprints-db",
        "--item-reports",
    ] {
        let block = text
            .split("\n\n")
            .find(|b| b.trim_start().starts_with(flag))
            .unwrap_or_else(|| panic!("missing {}", flag));
        assert!(block.contains("Default:"), "{} has no Default:", flag);
        assert!(block.contains("Example:"), "{} has no Example:", flag);
    }
    for s in [
        "revisions/rev-NN.pdf",
        "visual/",
        "objects.json",
        "embedded/",
        "PDFium",
        "never a verdict",
    ] {
        assert!(text.contains(s), "help must mention {}", s);
    }
    for args in [
        vec!["analyze", "--help"],
        vec!["fingerprints", "--help"],
        vec!["fingerprints", "learn", "--help"],
        vec!["fingerprints", "list", "--help"],
        vec!["fingerprints", "export", "--help"],
    ] {
        let o = bin(d.path()).args(&args).output().unwrap();
        let t = String::from_utf8(o.stdout).unwrap();
        assert!(
            t.contains("EXAMPLES:") || t.contains("Example:"),
            "{:?}",
            args
        );
        for l in t.lines() {
            assert!(l.chars().count() <= 100, "{:?}: line too long: {}", args, l);
        }
    }
}

#[test]
fn invalid_domain_values_are_usage_errors() {
    let d = tempfile::tempdir().unwrap();
    for bad in [
        ["--dpi", "10"],
        ["--max-pages", "0"],
        ["--visual-diff", "maybe"],
        ["--revisions", "all"],
        ["--item-reports", "some"],
        ["--worker-timeout", "0"],
        ["--worker-memory", "1"],
    ] {
        bin(d.path())
            .args(["analyze", "-i", "."])
            .args(bad)
            .assert()
            .code(2);
    }
    let out = tempfile::tempdir().unwrap();
    bin(d.path())
        .args(["analyze", "-i"])
        .arg(sample())
        .args([
            "--fingerprints-db",
            "no-such-db.json",
            "--progress",
            "none",
            "-o",
        ])
        .arg(out.path())
        .assert()
        .code(1)
        .stderr(predicate::str::contains("fingerprint database not found"));
}

#[test]
fn domain_options_switch_outputs_off_and_flagged_skips_ok_items() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    bin(d.path())
        .args(["analyze", "-i"])
        .arg(sample())
        .args([
            "--revisions",
            "none",
            "--visual-diff",
            "off",
            "--objects-dump",
            "off",
            "--extract-embedded",
            "off",
            "--item-reports",
            "flagged",
            "--worker-timeout",
            "60",
            "--worker-memory",
            "1024",
            "--dpi",
            "72",
            "--max-pages",
            "5",
            "--progress",
            "none",
            "-o",
        ])
        .arg(out.path())
        .assert()
        .code(3);
    let reports = only_dir(&out.path().join("reports"));
    let logs = only_dir(&out.path().join("logs"));
    let stats = json(&reports.join("statistics.json"));
    let items = stats["items"].as_array().unwrap();
    assert_eq!(items.len(), 7, "every item is still listed");
    assert_eq!(stats["pdf_overview"]["item_reports"], "flagged");
    let actions: Vec<serde_json::Value> = std::fs::read_to_string(logs.join("audit.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut skipped = 0;
    for it in items {
        let dir = reports.join(it["report_folder"].as_str().unwrap());
        assert!(!dir.join("revisions").exists(), "--revisions none");
        assert!(!dir.join("visual").exists(), "--visual-diff off");
        assert!(!dir.join("objects.json").exists(), "--objects-dump off");
        assert!(!dir.join("embedded").exists(), "--extract-embedded off");
        let ok = it["status"] == "ok";
        assert_eq!(it["reports_written"], !ok);
        assert_eq!(dir.join("technical.json").is_file(), !ok);
        assert_eq!(dir.join("summary.en.pdf").is_file(), !ok);
        assert_eq!(dir.join("custody.json").is_file(), !ok);
        let path = it["path"].as_str().unwrap();
        let skip_logged = actions
            .iter()
            .any(|a| a["action"] == "item_reports_skipped" && a["item"] == path);
        assert_eq!(skip_logged, ok, "skip of {} is audited", path);
        if ok {
            skipped += 1;
        }
    }
    assert!(
        skipped > 0,
        "the sample has items without findings above info"
    );
    assert_eq!(stats["pdf_overview"]["item_reports_skipped"], skipped);
    let html = std::fs::read_to_string(reports.join("statistics.en.html")).unwrap();
    assert!(html.contains("no report (no issues found)"));
    bin(d.path()).arg("verify-log").arg(&logs).assert().code(0);
}

// ---------------------------------------------------------------- report sections in en/es/fr

fn label(lang: &str, key: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../i18n/{}.json", lang));
    json(&p)["labels"][key].as_str().unwrap().to_string()
}

/// The template engine escapes these characters in text.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[test]
fn reports_have_pdf_sections_in_every_language() {
    let d = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    bin(d.path())
        .args(["analyze", "-i"])
        .arg(sample())
        .args(["--lang", "all", "--progress", "none", "-o"])
        .arg(out.path())
        .assert()
        .code(3);
    let reports = only_dir(&out.path().join("reports"));
    let item = std::fs::read_dir(reports.join("items"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().contains("signed-then-modified"))
        .unwrap();
    for lang in ["en", "es", "fr"] {
        let tech = std::fs::read_to_string(item.join(format!("technical.{}.html", lang))).unwrap();
        for key in [
            "revision_history",
            "timeline",
            "signatures",
            "fonts",
            "producer_fingerprints",
            "visual_comparison",
            "extracted_files",
        ] {
            assert!(
                tech.contains(&html_escape(&label(lang, key))),
                "technical.{}.html lacks {}",
                lang,
                key
            );
        }
        assert!(tech.contains("class=\"timeline\""));
        assert!(
            // minijinja escapes '/' in attribute values; browsers decode it.
            tech.contains("<img src=\"visual&#x2f;rev-02-vs-03-page-"),
            "thumbnails link to the PNGs"
        );
        let pdf = std::fs::read(item.join(format!("technical.{}.pdf", lang))).unwrap();
        assert!(pdf.starts_with(b"%PDF-") && pdf.ends_with(b"%%EOF\n"));
        let summary = std::fs::read_to_string(item.join(format!("summary.{}.html", lang))).unwrap();
        assert!(summary.contains(&html_escape(&label(lang, "history"))));
        let stats =
            std::fs::read_to_string(reports.join(format!("statistics.{}.html", lang))).unwrap();
        for key in [
            "pdf_overview",
            "pdfs_multi_revision",
            "pdfs_signed",
            "pdfs_online_editor",
        ] {
            assert!(
                stats.contains(&html_escape(&label(lang, key))),
                "statistics.{}.html lacks {}",
                lang,
                key
            );
        }
    }
    let summary = json(&item.join("summary.json"));
    assert_eq!(summary["history"]["saves"], 3);
    assert!(
        summary["history"]["text"]["en"]
            .as_str()
            .unwrap()
            .starts_with(
                "The document was saved 3 times; the last change was made 3 days after creation"
            ),
        "{}",
        summary["history"]["text"]["en"]
    );
    for lang in ["es", "fr"] {
        assert!(
            summary["history"]["text"][lang]
                .as_str()
                .unwrap()
                .contains('3')
        );
    }
    let tech = json(&item.join("technical.json"));
    let revs = tech["pdf"]["revisions"].as_array().unwrap();
    assert_eq!(revs.len(), 3);
    assert_eq!(revs[0]["start_offset"], 0);
    assert_eq!(revs[1]["signed_by"][0], "Signature1");
    assert!(revs[1]["since_previous_s"].is_i64());
    assert!(!tech["pdf"]["signatures"].as_array().unwrap().is_empty());
    assert!(
        tech["extracted_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rel_path"] == "revisions/rev-01.pdf" && f["hashes"]["sha256"].is_string())
    );
    let stats = json(&reports.join("statistics.json"));
    assert_eq!(stats["pdf_overview"]["pdfs"], 7);
    assert_eq!(stats["pdf_overview"]["multi_revision"], 3);
    assert_eq!(stats["pdf_overview"]["signed"], 1);
    assert_eq!(stats["pdf_overview"]["online_editor"], 1);
}

// ---------------------------------------------------------------- fingerprints learn / list / export

#[test]
fn fingerprints_learn_merges_and_is_audited() {
    let d = tempfile::tempdir().unwrap();
    let o = bin(d.path())
        .args(["fingerprints", "learn"])
        .arg(sample().join("original.pdf"))
        .args([
            "--label",
            "Lab ReportLab",
            "--category",
            "pdf_library",
            "--case",
            "LAB-1",
        ])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8(o.stdout).unwrap();
    let db_path = local_db(d.path());
    let after = sha256(&std::fs::read(&db_path).unwrap());
    assert!(text.contains("LEARNED local-lab-reportlab"));
    assert!(text.contains("sha256 before:     absent"));
    assert!(text.contains(&format!("sha256 after:      {}", after)));
    let db = json(&db_path);
    let e = &db["entries"][0];
    assert_eq!(e["category"], "pdf_library");
    assert!(
        e["producer_contains"][0]
            .as_str()
            .unwrap()
            .contains("ReportLab")
    );
    assert_eq!(e["traits"]["xref_kind"], "table");
    assert!(
        e["source"]
            .as_str()
            .unwrap()
            .contains("learned from case LAB-1")
    );

    // Same label again: merged, version bumped, audit chain continues.
    let o = bin(d.path())
        .args(["fingerprints", "learn"])
        .arg(sample().join("broken-xref.pdf"))
        .args(["--label", "Lab ReportLab", "--category", "pdf_library"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8(o.stdout).unwrap();
    assert!(text.contains("UPDATED local-lab-reportlab"));
    assert!(text.contains(&format!("sha256 before:     {}", after)));
    let db = json(&db_path);
    assert_eq!(db["version"], "2");
    assert_eq!(db["entries"].as_array().unwrap().len(), 1);
    let audit: Vec<serde_json::Value> =
        std::fs::read_to_string(db_path.with_file_name("fingerprints.audit.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[0]["action"], "fingerprints_learn");
    assert_eq!(audit[1]["seq"], 2);
    assert_eq!(audit[1]["prev_entry_hash"], audit[0]["entry_hash"]);
    assert_eq!(audit[1]["db_sha256_before"], audit[0]["db_sha256_after"]);
    assert_eq!(
        audit[0]["files"][0]["sha256"],
        sha256(&std::fs::read(sample().join("original.pdf")).unwrap())
    );

    // Failures leave the DB untouched.
    let before = std::fs::read(&db_path).unwrap();
    bin(d.path())
        .args(["fingerprints", "learn"])
        .arg(sample().join("original.pdf"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .args(["--label", "X", "--category", "office"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("not a PDF"));
    bin(d.path())
        .args(["fingerprints", "learn"])
        .arg(sample().join("original.pdf"))
        .args(["--label", "X", "--category", "spaceship"])
        .assert()
        .code(2);
    assert_eq!(std::fs::read(&db_path).unwrap(), before);
}

#[test]
fn fingerprints_list_and_export_then_analyze_uses_them() {
    let d = tempfile::tempdir().unwrap();
    bin(d.path())
        .args(["fingerprints", "list", "--db", "local"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("# local  absent"));
    bin(d.path())
        .args(["fingerprints", "export", "--out"])
        .arg(d.path().join("x.json"))
        .assert()
        .code(1);
    bin(d.path())
        .args(["fingerprints", "learn"])
        .arg(sample().join("original.pdf"))
        .args(["--label", "Lab ReportLab", "--category", "pdf_library"])
        .assert()
        .code(0);
    let db_sha = sha256(&std::fs::read(local_db(d.path())).unwrap());
    bin(d.path())
        .args(["fingerprints", "list"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("# starter  rakvor-starter"))
        .stdout(predicate::str::contains(format!("sha256 {}", db_sha)))
        .stdout(predicate::str::contains("local-lab-reportlab"));

    let export = d.path().join("lab.json");
    bin(d.path())
        .args(["fingerprints", "export", "--out"])
        .arg(&export)
        .assert()
        .code(0)
        .stdout(predicate::str::contains(db_sha.as_str()));
    assert_eq!(
        sha256(&std::fs::read(&export).unwrap()),
        db_sha,
        "the export is a byte copy of the local DB"
    );
    bin(d.path())
        .args(["fingerprints", "export", "--out"])
        .arg(&export)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("already exists"));
    let starter = d.path().join("starter.json");
    bin(d.path())
        .args(["fingerprints", "export", "--db", "starter", "--out"])
        .arg(&starter)
        .assert()
        .code(0);
    assert_eq!(json(&starter)["name"], "rakvor-starter");

    // analyze picks up the local DB and the exported copy (--fingerprints-db) and reports both.
    let out = tempfile::tempdir().unwrap();
    bin(d.path())
        .args(["analyze", "-i"])
        .arg(sample().join("original.pdf"))
        .args([
            "--visual-diff",
            "off",
            "--progress",
            "none",
            "--fingerprints-db",
        ])
        .arg(&export)
        .arg("-o")
        .arg(out.path())
        .assert()
        .code(0);
    let item = only_dir(&only_dir(&out.path().join("reports")).join("items"));
    let tech = json(&item.join("technical.json"));
    let dbs = tech["pdf"]["fingerprint_dbs"].as_array().unwrap();
    let local = dbs.iter().find(|x| x["role"] == "local").unwrap();
    assert_eq!(local["status"], "loaded");
    assert_eq!(local["sha256"], db_sha.as_str());
    let extra = dbs.iter().find(|x| x["role"] == "extra").unwrap();
    assert_eq!(extra["sha256"], db_sha.as_str());
    assert!(
        tech["pdf"]["fingerprint_matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["entry_id"] == "local-lab-reportlab")
    );
}
