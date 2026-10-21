//! Worker isolation, end to end with the built binary: PDFs are analysed in `rv-pdfauth worker` child
//! processes; a crash, a hang or a memory blow-up there becomes PARSER_FAILED and the run still finishes.
//! The faults are injected with RV_WORKER_TEST_FAULT, which only debug builds honour.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/visual")
        .join(name)
}

struct Run {
    code: i32,
    audit: Vec<Value>,
    stderr: String,
    log_dir: PathBuf,
    /// Evidence and output folders, removed when the Run is dropped.
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Run {
    fn actions(&self, action: &str) -> Vec<&Value> {
        self.audit
            .iter()
            .filter(|e| e["action"] == action)
            .collect()
    }
    /// item_analyzed entry of the item whose path ends with `name`.
    fn analyzed(&self, name: &str) -> &Value {
        self.actions("item_analyzed")
            .into_iter()
            .find(|e| e["item"].as_str().is_some_and(|p| p.ends_with(name)))
            .unwrap_or_else(|| panic!("no item_analyzed for {name}"))
    }
}

fn scan(fault: Option<&str>, extra_env: &[(&str, &str)]) -> Run {
    let ev = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    std::fs::copy(fixture("two-revisions.pdf"), ev.path().join("contract.pdf")).unwrap();
    std::fs::write(
        ev.path().join("note.txt"),
        "plain text, analysed in process",
    )
    .unwrap();
    let mut c = Command::cargo_bin("rv-pdfauth").unwrap();
    c.env("RV_NO_TIMESTAMP", "1")
        .env_remove("RV_WORKER_TEST_FAULT")
        .env_remove("RV_WORKER_TEST_TIMEOUT_S");
    if let Some(f) = fault {
        c.env("RV_WORKER_TEST_FAULT", f);
    }
    for (k, v) in extra_env {
        c.env(k, v);
    }
    let o = c
        .args(["scan", "-i"])
        .arg(ev.path())
        .arg("--out")
        .arg(out.path())
        .args(["--progress", "none", "--no-timestamp"])
        .timeout(std::time::Duration::from_secs(300))
        .output()
        .unwrap();
    let logs = out.path().join("logs");
    let log_dir = std::fs::read_dir(&logs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let audit = std::fs::read_to_string(log_dir.join("audit.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Run {
        code: o.status.code().unwrap_or(-1),
        audit,
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        log_dir,
        _dirs: (ev, out),
    }
}

fn assert_parser_failed(r: &Run, reason: &str) {
    assert_eq!(
        r.code, 3,
        "a high finding means exit 3; stderr: {}",
        r.stderr
    );
    let pdf = r.analyzed("contract.pdf");
    let f = pdf["details"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "PARSER_FAILED")
        .unwrap_or_else(|| panic!("no PARSER_FAILED in {pdf}"));
    assert_eq!(f["severity"], "high");
    assert_eq!(f["params"]["reason"], reason, "{pdf}");
    let w = &pdf["details"]["analysis"]["worker"];
    assert_eq!(w["status"], "failed");
    assert_eq!(w["reason"], reason);
    assert!(w["exit_code"].is_string());
    assert!(w["stderr_tail"].is_string());
    // The run went on: the text file was analysed and the run finished normally.
    assert_eq!(r.analyzed("note.txt")["details"]["status"], "ok");
    assert_eq!(r.actions("run_finished").len(), 1);
    let fin = r.actions("run_finished")[0];
    assert_eq!(fin["details"]["items"], 2);
    assert_eq!(fin["details"]["cancelled"], false);
    // The chain is intact.
    let v = Command::cargo_bin("rv-pdfauth")
        .unwrap()
        .arg("verify-log")
        .arg(&r.log_dir)
        .output()
        .unwrap();
    assert!(v.status.success(), "{}", String::from_utf8_lossy(&v.stdout));
}

#[test]
fn worker_success_outputs_are_audited() {
    let r = scan(None, &[]);
    assert!(r.code == 0 || r.code == 3, "{}", r.stderr);
    let pdf = r.analyzed("contract.pdf");
    let summary = &pdf["details"]["analysis"];
    assert_eq!(summary["worker"]["status"], "completed", "{pdf}");
    assert_eq!(summary["worker"]["protocol"], "rv-pdfauth-worker/1");
    assert!(summary["fingerprint_dbs"].is_array());
    let started = r.actions("analysis_started");
    assert!(
        started
            .iter()
            .any(|e| e["details"]["isolation"] == "worker_process")
    );
    let outputs = r.actions("output_written");
    let files: Vec<&str> = outputs
        .iter()
        .map(|e| e["details"]["file"].as_str().unwrap())
        .collect();
    assert!(
        files.iter().any(|f| f.ends_with("/revisions/rev-01.pdf")),
        "{files:?}"
    );
    assert!(files.iter().any(|f| f.ends_with("/revisions/rev-02.pdf")));
    assert!(files.iter().any(|f| f.ends_with("/objects.json")));
    for e in &outputs {
        assert_eq!(e["details"]["hashes"]["sha256"].as_str().unwrap().len(), 64);
    }
    if summary["visual"]["status"] == "done" {
        assert!(
            files
                .iter()
                .any(|f| f.ends_with("/visual/rev-01-vs-02-page-001.png")),
            "{files:?}"
        );
        assert!(
            pdf["details"]["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["code"] == "VISUAL_CHANGE_BETWEEN_REVISIONS")
        );
    } else {
        eprintln!("PDFium not found by the worker: visual part not checked (set RV_PDFIUM_PATH)");
        assert_eq!(summary["visual"]["status"], "skipped");
    }
}

#[test]
fn worker_crash_yields_parser_failed_and_partial_output_is_audited() {
    if !cfg!(debug_assertions) {
        eprintln!("skipping: fault injection exists in debug builds only");
        return;
    }
    let r = scan(Some("crash"), &[]);
    assert_parser_failed(&r, "crashed");
    let partial: Vec<&Value> = r
        .actions("output_written")
        .into_iter()
        .filter(|e| e["details"]["kind"] == "partial_output")
        .collect();
    assert_eq!(
        partial.len(),
        1,
        "partial output must be hashed and audited"
    );
    assert!(
        partial[0]["details"]["file"]
            .as_str()
            .unwrap()
            .ends_with("/partial-before-crash.bin")
    );
}

#[test]
fn worker_hang_is_killed_at_the_timeout() {
    if !cfg!(debug_assertions) {
        eprintln!("skipping: fault injection exists in debug builds only");
        return;
    }
    let t = std::time::Instant::now();
    let r = scan(Some("hang"), &[("RV_WORKER_TEST_TIMEOUT_S", "2")]);
    assert_parser_failed(&r, "timeout");
    assert!(
        t.elapsed().as_secs() < 120,
        "the worker was not killed in time"
    );
    let w = &r.analyzed("contract.pdf")["details"]["analysis"]["worker"];
    assert_eq!(w["timeout_s"], 2);
}

#[test]
fn worker_memory_limit_yields_parser_failed() {
    if !cfg!(debug_assertions) {
        eprintln!("skipping: fault injection exists in debug builds only");
        return;
    }
    let r = scan(Some("oom"), &[]);
    assert_parser_failed(&r, "memory_limit");
}
