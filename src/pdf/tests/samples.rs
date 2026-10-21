//! examples/sample/expected_findings.json lists, per sample, the exact set of finding codes the
//! PDF layer must produce (core-only codes such as PARSER_FAILED excluded).

mod common;
use std::collections::BTreeMap;

#[test]
fn samples_produce_exactly_the_expected_findings() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/sample/expected_findings.json");
    let text = std::fs::read_to_string(&path).expect("expected_findings.json");
    let expected: BTreeMap<String, Vec<String>> = serde_json::from_str(&text).expect("valid JSON");
    assert!(expected.len() >= 7);
    let mut failures = Vec::new();
    for (file, want) in &expected {
        let mut want = want.clone();
        want.sort();
        let got = common::codes(&common::sample(file));
        if got != want {
            failures.push(format!("{}: expected {:?}, got {:?}", file, want, got));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_sample_in_the_folder_is_listed() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sample/evidence");
    let text = std::fs::read_to_string(dir.join("../expected_findings.json")).unwrap();
    let expected: BTreeMap<String, Vec<String>> = serde_json::from_str(&text).unwrap();
    for e in std::fs::read_dir(&dir).unwrap() {
        let name = e.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            expected.contains_key(&name),
            "{} missing from expected_findings.json",
            name
        );
    }
}

#[test]
fn sample_revisions_match_their_story() {
    let r = |f: &str| rv_pdfauth_pdf::analyze(&common::sample(f), &Default::default());
    let e = r("edited-incremental.pdf");
    assert_eq!(e.revisions.len(), 3);
    assert_eq!(e.revisions[1].pages_changed, vec![1]);
    assert!(e.revisions[2].change_kind.contains(&"other".to_string()));
    let o = r("original.pdf");
    assert_eq!(o.revisions.len(), 1);
    assert!(
        o.findings
            .iter()
            .all(|f| f.severity == rv_pdfauth_pdf::Severity::Info)
    );
    let c = r("crash.pdf");
    assert!(c.is_pdf);
    assert!(!c.warnings.is_empty());
}
