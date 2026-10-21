//! Cross-tool test: a case folder registered by the intake tool (rv-hashinv) is verified and extended by
//! rv-pdfauth (analysis role).
//!
//! The intake side is reproduced here with the shared case code (`rv_pdfauth_core::case`, ported unchanged from
//! the forge reference) and `CaseRole::Intake` passed explicitly, writing exactly what rv-hashinv writes:
//! case.json, evidence/registry.json + .csv, `case_created` / `run_started` / `evidence_registered` /
//! `run_finished` in case-log.jsonl and the run folder runs/rv-hashinv/<RUN-ID>/. CI therefore never needs the
//! rv-hashinv repository; tests/toolkit_e2e.sh runs the same scenario with the real rv-hashinv binary.

use assert_cmd::Command;
use predicates::prelude::*;
use rv_pdfauth_core::analyzer::{DomainOptions, Isolation};
use rv_pdfauth_core::{CaseRole, HashAlgo, RunConfig, case, run};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const SAMPLE_ITEMS: usize = 7;

fn bin() -> Command {
    let mut c = Command::cargo_bin("rv-pdfauth").unwrap();
    c.env("RV_NO_TIMESTAMP", "1");
    c
}

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sample/evidence")
}

/// A working copy of examples/sample/evidence (the tests modify it).
fn evidence_copy(dir: &Path) -> PathBuf {
    let ev = dir.join("evidence-copy");
    std::fs::create_dir_all(&ev).unwrap();
    for e in std::fs::read_dir(sample()).unwrap() {
        let p = e.unwrap().path();
        std::fs::copy(&p, ev.join(p.file_name().unwrap())).unwrap();
    }
    ev
}

/// Intake exactly as rv-hashinv does it (tool name, role, SHA-256 + MD5, jurisdiction co). The PDF analysis
/// that the shared pipeline also runs is reduced to the minimum and isolated in the worker process.
fn intake_as_rv_hashinv(case: &Path, evidence: &Path) {
    let cfg = RunConfig {
        inputs: vec![evidence.to_path_buf()],
        recursive: true,
        algorithms: vec![HashAlgo::Sha256, HashAlgo::Md5],
        actor: "intake-examiner".into(),
        case_id: Some("CASE-TK-1".into()),
        jurisdiction: "co".into(),
        frontend: "cli".into(),
        tool_name: "rv-hashinv".into(),
        tool_version: "0.1.0".into(),
        case_dir: Some(case.to_path_buf()),
        case_role: CaseRole::Intake,
        domain: DomainOptions {
            extract_revisions: false,
            visual_diff: false,
            objects_dump: false,
            extract_embedded: false,
            isolation: Isolation::Subprocess {
                exe: PathBuf::from(env!("CARGO_BIN_EXE_rv-pdfauth")),
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let s = run(&cfg, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let t = s.case.unwrap().tally;
    assert_eq!(
        (t.registered, t.added),
        (SAMPLE_ITEMS as u64, SAMPLE_ITEMS as u64)
    );
}

fn analyze(case: &Path) -> Command {
    let mut c = bin();
    c.args([
        "analyze",
        "--progress",
        "none",
        "--visual-diff",
        "off",
        "--case-dir",
    ])
    .arg(case);
    c
}

/// statistics.json of the run whose summary is `stdout` (its "logs:" line names <CASE>/runs/rv-pdfauth/<RUN-ID>/logs).
fn run_statistics(stdout: &[u8]) -> Value {
    let text = String::from_utf8_lossy(stdout);
    let logs = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("logs:"))
        .expect("logs line")
        .trim();
    let run_dir = PathBuf::from(logs).parent().unwrap().to_path_buf();
    assert!(run_dir.parent().unwrap().ends_with("runs/rv-pdfauth"));
    let p = run_dir.join("reports/statistics.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn codes(stats: &Value, file: &str) -> Vec<String> {
    stats["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"].as_str().unwrap().ends_with(file))
        .map(|i| {
            i["findings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn rv_hashinv_intake_then_rv_pdfauth_analysis() {
    let tmp = tempfile::tempdir().unwrap();
    let case = tmp.path().join("CASE-TK-1");
    let ev = evidence_copy(tmp.path());
    intake_as_rv_hashinv(&case, &ev);
    let registry = case::Case::open(&case).unwrap();
    assert_eq!(registry.items.len(), SAMPLE_ITEMS);
    assert!(
        registry
            .items
            .iter()
            .all(|i| i.registered_by.tool == "rv-hashinv")
    );

    // 1. Every registered item, re-hashed with BLAKE3 only: SHA-256 is added (case_algorithm_added) and all
    //    seven items verify. The sample has PDF findings of high severity, hence exit 3.
    let out = analyze(&case)
        .args(["--hash", "blake3"])
        .assert()
        .code(3)
        .stdout(predicate::str::contains(format!(
            "case:   CASE-TK-1 (analysis) verified {}, mismatched 0, not in registry 0",
            SAMPLE_ITEMS
        )))
        .get_output()
        .stdout
        .clone();
    let stats = run_statistics(&out);
    assert_eq!(stats["case"]["counts"]["verified"], SAMPLE_ITEMS as u64);
    assert!(stats["items"].as_array().unwrap().iter().all(|i| {
        i["evidence_id"]
            .as_str()
            .is_some_and(|id| id.starts_with('E'))
    }));
    bin()
        .arg("verify-case")
        .arg(&case)
        .assert()
        .code(0)
        .stdout(predicate::str::contains(format!(
            "{} registered items, 2 runs verified",
            SAMPLE_ITEMS
        )));

    // 2. A tampered copy of a registered file → EVIDENCE_HASH_MISMATCH_WITH_CASE (high); a file that was never
    //    registered → EVIDENCE_NOT_IN_CASE_REGISTRY (medium). The analysis still runs on both.
    let mut bytes = std::fs::read(ev.join("original.pdf")).unwrap();
    bytes.extend_from_slice(b"\n% one byte too many\n");
    std::fs::write(ev.join("original.pdf"), bytes).unwrap();
    std::fs::write(
        tmp.path().join("unregistered.pdf"),
        [
            std::fs::read(sample().join("original.pdf")).unwrap(),
            b"\n% new\n".to_vec(),
        ]
        .concat(),
    )
    .unwrap();
    let out = analyze(&case)
        .arg("-i")
        .arg(ev.join("original.pdf"))
        .arg("-i")
        .arg(tmp.path().join("unregistered.pdf"))
        .assert()
        .code(3)
        .stdout(predicate::str::contains("mismatched 1, not in registry 1"))
        .get_output()
        .stdout
        .clone();
    let stats = run_statistics(&out);
    assert!(
        codes(&stats, "original.pdf").contains(&"EVIDENCE_HASH_MISMATCH_WITH_CASE".to_string())
    );
    assert!(
        codes(&stats, "unregistered.pdf").contains(&"EVIDENCE_NOT_IN_CASE_REGISTRY".to_string())
    );
    let fin: Value = serde_json::from_str(
        std::fs::read_to_string(case.join(case::CASE_LOG))
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(fin["action"], "run_finished");
    assert_eq!(fin["details"]["verification"]["mismatched"], 1);
    assert_eq!(fin["details"]["verification"]["unregistered"], 1);
    bin().arg("verify-case").arg(&case).assert().code(0);

    // 3. An edited case log: verify-case exit 3 and rv-pdfauth refuses to extend the case.
    let log = case.join(case::CASE_LOG);
    let text = std::fs::read_to_string(&log).unwrap();
    std::fs::write(&log, text.replacen("intake-examiner", "mallory", 1)).unwrap();
    bin()
        .arg("verify-case")
        .arg(&case)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("BROKEN"));
    analyze(&case)
        .assert()
        .code(3)
        .stderr(predicate::str::contains("never extended"));
}
