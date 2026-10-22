//! End-to-end tests of the rv-pdfauth binary: the contract every Rakvor CLI must meet.

use assert_cmd::Command;
use predicates::prelude::*;
use rv_pdfauth_core::{CASE_ROLE, CaseRole, HashAlgo, RunConfig, run};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const TOOL: &str = rv_pdfauth_core::TOOL_NAME;

fn bin() -> Command {
    let mut c = Command::cargo_bin("rv-pdfauth").unwrap();
    c.env("RV_NO_TIMESTAMP", "1"); // offline by default; trust_* tests use a local test TSA
    c
}

/// Starts tests/fake_tsa.py (needs Python with pyhanko). Returns None to skip when unavailable.
fn test_pki(dir: &Path) -> Option<(std::process::Child, String)> {
    let py = std::env::var("RV_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fake_tsa.py");
    let ok = std::process::Command::new(&py)
        .args(["-c", "import pyhanko"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!(
            "skipping trust test: {} has no pyhanko (pip install pyhanko)",
            py
        );
        return None;
    }
    let mut child = std::process::Command::new(&py)
        .arg(&script)
        .arg("serve")
        .arg(dir)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut line = String::new();
    use std::io::BufRead;
    std::io::BufReader::new(child.stdout.take()?)
        .read_line(&mut line)
        .ok()?;
    Some((child, line.trim().to_string()))
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

#[test]
fn help_has_every_required_section() {
    let out = bin().args(["analyze", "--help"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    for section in [
        "DESCRIPTION:",
        "Options:",
        "EXAMPLES:",
        "EXIT CODES:",
        "FILES WRITTEN:",
        "FORENSIC NOTES:",
    ] {
        assert!(text.contains(section), "missing {}", section);
    }
    for flag in [
        "--input",
        "--input-list",
        "--recursive",
        "--hash",
        "--out",
        "--lang",
        "--actor",
        "--case",
        "--progress",
        "--case-dir",
        "--evidence",
    ] {
        assert!(text.contains(flag), "missing {}", flag);
    }
    // every option documents a default and an example
    assert!(
        text.matches("Default:").count() >= 9,
        "each option needs a Default: line"
    );
    assert!(
        text.matches("Example:").count() >= 9,
        "each option needs an Example: line"
    );
}

#[test]
fn three_hash_algorithms_are_a_usage_error() {
    bin()
        .args(["analyze", "-i", "."])
        .args(["--hash", "md5,sha1,sha256"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("at most 2"));
}

#[test]
fn unknown_language_and_algorithm_are_usage_errors() {
    bin()
        .args(["analyze", "-i", ".", "--lang", "de"])
        .assert()
        .code(2);
    bin()
        .args(["analyze", "-i", ".", "--hash", "crc32"])
        .assert()
        .code(2);
}

#[test]
fn missing_input_is_a_runtime_error_and_is_audited() {
    let out = tempfile::tempdir().unwrap();
    bin()
        .args(["analyze", "-i", "does-not-exist", "-o"])
        .arg(out.path())
        .args(["--progress", "none"])
        .assert()
        .code(1);
    let log = only_dir(&out.path().join("logs")).join("audit.jsonl");
    let text = std::fs::read_to_string(log).unwrap();
    assert!(text.contains("\"run_failed\""));
}

#[test]
fn bulk_scan_writes_reports_logs_and_verifiable_chain() {
    let out = tempfile::tempdir().unwrap();
    bin()
        .args(["analyze", "-i"])
        .arg(sample())
        .args([
            "--hash",
            "sha256,blake3",
            "--lang",
            "all",
            "--case",
            "T-1",
            "--actor",
            "ci",
            "--progress",
            "none",
            "-o",
        ])
        .arg(out.path())
        .assert()
        .code(3)
        .stdout(predicate::str::contains("items: 7"));
    let logs = only_dir(&out.path().join("logs"));
    let reports = only_dir(&out.path().join("reports"));
    for f in ["audit.jsonl", "audit.log", "hashes.csv", "environment.json"] {
        assert!(logs.join(f).is_file(), "{}", f);
    }
    for l in ["en", "es", "fr"] {
        for ext in ["html", "pdf"] {
            assert!(reports.join(format!("statistics.{}.{}", l, ext)).is_file());
        }
    }
    let items: Vec<_> = std::fs::read_dir(reports.join("items")).unwrap().collect();
    assert_eq!(items.len(), 7, "one folder per item");
    for item in items {
        let d = item.unwrap().path();
        for f in [
            "technical.json",
            "summary.json",
            "technical.es.pdf",
            "summary.fr.html",
        ] {
            assert!(d.join(f).is_file(), "{} in {}", f, d.display());
        }
        let pdf = std::fs::read(d.join("technical.en.pdf")).unwrap();
        assert!(pdf.starts_with(b"%PDF-") && pdf.ends_with(b"%%EOF\n"));
    }
    bin()
        .arg("verify-log")
        .arg(&logs)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("chain intact"));

    // Tampering with one entry is detected.
    let p = logs.join("audit.jsonl");
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(
        &p,
        text.replacen("\"actor\":\"ci\"", "\"actor\":\"mallory\"", 1),
    )
    .unwrap();
    bin()
        .arg("verify-log")
        .arg(&p)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("BROKEN"));
}

#[test]
fn jsonl_progress_is_one_event_per_line_with_elapsed_time() {
    let out = tempfile::tempdir().unwrap();
    let o = bin()
        .args(["analyze", "-i"])
        .arg(sample())
        .args(["--progress", "jsonl", "-o"])
        .arg(out.path())
        .output()
        .unwrap();
    let text = String::from_utf8(o.stdout).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect();
    assert_eq!(events.first().unwrap()["event"], "run_started");
    assert_eq!(events.last().unwrap()["event"], "run_finished");
    let elapsed: Vec<u64> = events
        .iter()
        .filter(|e| e["event"] == "progress")
        .map(|e| e["elapsed_ms"].as_u64().unwrap())
        .collect();
    assert!(
        !elapsed.is_empty() && elapsed.windows(2).all(|w| w[0] <= w[1]),
        "elapsed_ms must be monotonic"
    );
}

#[test]
fn evidence_is_not_modified() {
    let ev = tempfile::tempdir().unwrap();
    let f = ev.path().join("a.bin");
    std::fs::write(&f, b"evidence").unwrap();
    let before = std::fs::metadata(&f).unwrap().modified().unwrap();
    let out = tempfile::tempdir().unwrap();
    bin()
        .args(["analyze", "-i"])
        .arg(&f)
        .args(["--progress", "none", "-o"])
        .arg(out.path())
        .assert()
        .code(0);
    assert_eq!(std::fs::read(&f).unwrap(), b"evidence");
    assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), before);
}

#[test]
fn executable_under_another_name_exits_3() {
    let ev = tempfile::tempdir().unwrap();
    std::fs::write(ev.path().join("invoice.pdf"), b"MZ\x90\x00\x03\x00\x00\x00").unwrap();
    let out = tempfile::tempdir().unwrap();
    bin()
        .args(["analyze", "-i"])
        .arg(ev.path())
        .args(["--progress", "none", "-o"])
        .arg(out.path())
        .assert()
        .code(3);
}

#[test]
fn custody_form_has_identification_fields_for_both_jurisdictions() {
    let ev = tempfile::tempdir().unwrap();
    std::fs::write(ev.path().join("notes.txt"), b"plain text evidence").unwrap();
    for (j, lang, words) in [
        (
            "co",
            "es",
            [
                "Cargo",
                "Tipo de documento de identidad",
                "Testigo",
                "Auxiliar",
            ],
        ),
        (
            "fr",
            "fr",
            [
                "Qualité / fonction",
                "Type de pièce d'identité",
                "Témoin",
                "Assistant",
            ],
        ),
    ] {
        let out = tempfile::tempdir().unwrap();
        bin()
            .args(["analyze", "-i"])
            .arg(ev.path())
            .args([
                "--jurisdiction",
                j,
                "--actor",
                "Ana Ruiz",
                "--progress",
                "none",
                "-o",
            ])
            .arg(out.path())
            .assert()
            .code(0);
        let item = only_dir(&only_dir(&out.path().join("reports")).join("items"));
        let html = std::fs::read_to_string(item.join(format!("custody.{}.html", lang))).unwrap();
        let html = html
            .replace("&#x27;", "'")
            .replace("&#39;", "'")
            .replace("&#x2f;", "/");
        for w in words {
            assert!(html.contains(w), "{} form lacks {:?}", j, w);
        }
        let en = std::fs::read_to_string(item.join("custody.en.html"))
            .unwrap()
            .replace("&#x2f;", "/");
        for w in [
            "Position / capacity",
            "ID document type",
            "ID document number",
            "Witness",
            "Assistant",
            "Examiner: Ana Ruiz",
        ] {
            assert!(en.contains(w), "{} English form lacks {:?}", j, w);
        }
        let pdf = std::fs::read(item.join(format!("custody.{}.pdf", lang))).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(item.join("custody.json")).unwrap()).unwrap();
        assert_eq!(json["examiner_identity"]["name"], "Ana Ruiz");
        for k in ["position", "id_type", "id_number"] {
            assert!(json["examiner_identity"][k].is_null(), "{}", k);
        }
        assert_eq!(json["assistants"], serde_json::json!([]));
        assert_eq!(json["witnesses"], serde_json::json!([]));
    }
}

#[test]
fn self_test_passes() {
    bin()
        .arg("self-test")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("FAIL").not());
}

#[test]
fn usage_doc_matches_help() {
    let generated = bin().arg("docs-cli").output().unwrap().stdout;
    let doc = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/usage-cli.md");
    let committed = std::fs::read(&doc).unwrap_or_default();
    assert!(
        committed == generated,
        "docs/usage-cli.md is out of date: run `cargo run -p rv-pdfauth-cli -- docs-cli > docs/usage-cli.md`"
    );
}

#[test]
fn trust_timestamp_signature_and_custody_with_test_pki() {
    let pki = tempfile::tempdir().unwrap();
    let Some((mut tsa, url)) = test_pki(pki.path()) else {
        return;
    };
    let out = tempfile::tempdir().unwrap();
    bin()
        .env_remove("RV_NO_TIMESTAMP")
        .env("NO_PROXY", "127.0.0.1")
        .env("RV_SIGN_PASSWORD", "test")
        .args(["analyze", "-i"])
        .arg(sample())
        .args([
            "--jurisdiction",
            "fr",
            "--tsa-url",
            &url,
            "--progress",
            "none",
            "--sign-cert",
        ])
        .arg(pki.path().join("examiner.p12"))
        .arg("-o")
        .arg(out.path())
        .assert()
        .code(3)
        .stdout(predicate::str::contains("timestamp: ok"));
    let logs = only_dir(&out.path().join("logs"));
    // The TSA's CMS signature is checked; the chain only with --tsa-ca.
    bin()
        .arg("verify-log")
        .arg(&logs)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("covers entry"))
        .stdout(predicate::str::contains("TSA signature valid"))
        .stdout(predicate::str::contains("chain not validated"));
    bin()
        .arg("verify-log")
        .arg(&logs)
        .arg("--tsa-ca")
        .arg(pki.path().join("ca.pem"))
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "chain validated to O=Rakvor TEST,CN=Rakvor TEST CA",
        ));
    // Another CA does not vouch for this TSA.
    let other = tempfile::tempdir().unwrap();
    let py = std::env::var("RV_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
    let made = std::process::Command::new(&py)
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fake_tsa.py"))
        .arg("certs")
        .arg(other.path())
        .status()
        .unwrap();
    assert!(made.success());
    bin()
        .arg("verify-log")
        .arg(&logs)
        .arg("--tsa-ca")
        .arg(other.path().join("ca.pem"))
        .assert()
        .code(3)
        .stdout(predicate::str::contains("chain not validated"));
    bin()
        .arg("verify-log")
        .arg(&logs)
        .arg("--tsa-ca")
        .arg(other.path().join("missing.pem"))
        .assert()
        .code(1);
    let item = only_dir(&only_dir(&out.path().join("reports")).join("items"));
    for f in [
        "custody.json",
        "custody.fr.pdf",
        "custody.fr.html",
        "custody.en.pdf",
    ] {
        assert!(item.join(f).is_file(), "{}", f);
    }
    let pdf = std::fs::read(item.join("technical.en.pdf")).unwrap();
    assert!(String::from_utf8_lossy(&pdf).contains("/adbe.pkcs7.detached"));
    // Tampering with the token is detected.
    let tsr = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "tsr"))
        .unwrap();
    let mut b = std::fs::read(&tsr).unwrap();
    // The last byte of a token is the last byte of the TSA's RSA signature value.
    let last = b.len() - 1;
    b[last] ^= 0xff;
    std::fs::write(&tsr, &b).unwrap();
    bin()
        .arg("verify-log")
        .arg(&logs)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("missing or altered"));
    // Even with the recorded token digest updated to match, the signature check catches it.
    let meta_path = tsr.with_extension("json");
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    meta["token_sha256"] =
        rv_pdfauth_core::hash::hash_bytes(&b, rv_pdfauth_core::HashAlgo::Sha256).into();
    std::fs::write(&meta_path, meta.to_string()).unwrap();
    bin()
        .arg("verify-log")
        .arg(&logs)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("signature value does not verify"));
    // Offline run: pending, then added later with `timestamp`.
    let out2 = tempfile::tempdir().unwrap();
    bin()
        .env_remove("RV_NO_TIMESTAMP")
        .args(["analyze", "-i"])
        .arg(sample())
        .args([
            "--tsa-url",
            "http://127.0.0.1:9/tsr",
            "--progress",
            "none",
            "-o",
        ])
        .arg(out2.path())
        .assert()
        .code(3)
        .stdout(predicate::str::contains("timestamp: pending"));
    let logs2 = only_dir(&out2.path().join("logs"));
    bin()
        .env("NO_PROXY", "127.0.0.1")
        .args(["timestamp"])
        .arg(&logs2)
        .args(["--tsa-url", &url])
        .assert()
        .code(0);
    bin()
        .arg("verify-log")
        .arg(&logs2)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("covers entry"));
    let _ = tsa.kill();
}

fn case_analyze(case: &Path) -> Command {
    let mut c = bin();
    c.args(["analyze", "--progress", "none", "--case-dir"])
        .arg(case);
    c
}

/// Analysis-role tools need registered evidence first: an intake run made through the core with the role set
/// explicitly (what the intake tool, rv-hashinv, does). Intake tools register through their own CLI.
fn seed_registry(case: &Path, case_id: &str, evidence: &Path) {
    let cfg = RunConfig {
        inputs: vec![evidence.to_path_buf()],
        algorithms: vec![HashAlgo::Sha256, HashAlgo::Md5],
        actor: "seed".into(),
        case_id: Some(case_id.into()),
        jurisdiction: "fr".into(),
        case_dir: Some(case.to_path_buf()),
        case_role: CaseRole::Intake,
        domain: light_analysis(),
        ..Default::default()
    };
    run(&cfg, &mut |_| {}, &AtomicBool::new(false)).unwrap();
}

/// PDF analysis without revision extraction, rendering or dumps (seeding only needs the hashes), in the
/// worker process so a hostile sample can never take the test process down.
fn light_analysis() -> rv_pdfauth_core::analyzer::DomainOptions {
    rv_pdfauth_core::analyzer::DomainOptions {
        extract_revisions: false,
        visual_diff: false,
        objects_dump: false,
        extract_embedded: false,
        isolation: rv_pdfauth_core::analyzer::Isolation::Subprocess {
            exe: PathBuf::from(env!("CARGO_BIN_EXE_rv-pdfauth")),
        },
        ..Default::default()
    }
}

/// Two clean sample PDFs (no finding above info), so exit codes only reflect the case checks.
fn clean_evidence(dir: &Path) -> PathBuf {
    let ev = dir.join("evidence");
    std::fs::create_dir_all(&ev).unwrap();
    std::fs::copy(sample().join("original.pdf"), ev.join("a-original.pdf")).unwrap();
    std::fs::copy(
        sample().join("original.pdf"),
        ev.join("b-original-copy.pdf"),
    )
    .unwrap();
    ev
}

/// The run folder printed on the "logs:" line of the summary.
fn run_folder(stdout: &[u8]) -> PathBuf {
    let text = String::from_utf8_lossy(stdout);
    let logs = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("logs:"))
        .expect("logs line")
        .trim();
    PathBuf::from(logs).parent().unwrap().to_path_buf()
}

#[test]
fn case_folder_analysis_verify_case_and_tampering() {
    assert_eq!(
        CASE_ROLE,
        CaseRole::Analysis,
        "rv-pdfauth is an analysis tool"
    );
    let tmp = tempfile::tempdir().unwrap();
    let case = tmp.path().join("CASE-1");
    let ev = clean_evidence(tmp.path());
    seed_registry(&case, "CASE-1", &ev);
    let out = case_analyze(&case)
        .args(["-i"])
        .arg(&ev)
        .args([
            "--case",
            "CASE-1",
            "--hash",
            "sha256,md5",
            "--jurisdiction",
            "co",
        ])
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "case:   CASE-1 (analysis) verified 2, mismatched 0, not in registry 0",
        ))
        .get_output()
        .stdout
        .clone();
    assert!(!case.join("case-log.lock").exists(), "lock released");
    let run_dir = run_folder(&out);
    assert!(run_dir.starts_with(case.join("runs").join(TOOL)));
    assert!(
        run_dir.join("logs/audit.jsonl").is_file()
            && run_dir.join("reports/statistics.en.html").is_file()
    );
    // The case's jurisdiction (fr, set at creation) overrides --jurisdiction co for the custody form.
    let item = only_dir(&run_dir.join("reports/items"));
    let custody = std::fs::read_to_string(item.join("custody.fr.html")).unwrap();
    assert!(custody.contains("E000") && custody.contains("CASE-1"));
    bin()
        .arg("verify-case")
        .arg(&case)
        .assert()
        .code(0)
        .stdout(predicate::str::contains(
            "2 registered items, 2 runs verified",
        ));
    // The case id may be left out once the case exists; no input = every registered item.
    case_analyze(&case)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("items: 2").and(predicate::str::contains("verified 2")));
    case_analyze(&case)
        .args(["--evidence", "e2"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("items: 1").and(predicate::str::contains("verified 1")));
    bin().arg("verify-case").arg(&case).assert().code(0);

    // Usage errors: another case's folder, unknown or malformed id, --evidence without --case-dir,
    // creation without --case, a case.json of another schema.
    case_analyze(&case)
        .args(["--case", "OTHER"])
        .assert()
        .code(2);
    case_analyze(&case)
        .args(["--evidence", "E0099"])
        .assert()
        .code(2);
    case_analyze(&case)
        .args(["--evidence", "7"])
        .assert()
        .code(2);
    bin()
        .args(["analyze", "--evidence", "E0001"])
        .assert()
        .code(2);
    case_analyze(&tmp.path().join("NEW"))
        .arg("-i")
        .arg(&ev)
        .assert()
        .code(2);
    let odd = tmp.path().join("ODD");
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(odd.join("case.json"), r#"{"schema":"other/9"}"#).unwrap();
    case_analyze(&odd).arg("-i").arg(&ev).assert().code(2);
    bin().arg("verify-case").arg(tmp.path()).assert().code(1);

    // A tampered case log: verify-case exit 3, and the tool does not extend the case.
    let log = case.join("case-log.jsonl");
    let original = std::fs::read_to_string(&log).unwrap();
    std::fs::write(&log, original.replacen("CASE-1", "CASE-X", 1)).unwrap();
    bin()
        .arg("verify-case")
        .arg(&case)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("BROKEN"));
    let runs = |c: &Path| {
        std::fs::read_dir(c.join("runs").join(TOOL))
            .unwrap()
            .count()
    };
    let before = runs(&case);
    case_analyze(&case).arg("-i").arg(&ev).assert().code(3);
    assert_eq!(runs(&case), before, "no run folder in a broken case");
    std::fs::write(&log, &original).unwrap();
    // A tampered registry: detected through the digest recorded in the case log.
    let reg = case.join("evidence/registry.json");
    let text = std::fs::read_to_string(&reg).unwrap();
    std::fs::write(&reg, text.replacen("\"size\": ", "\"size\": 1", 1)).unwrap();
    bin()
        .arg("verify-case")
        .arg(&case)
        .assert()
        .code(3)
        .stdout(predicate::str::contains("registry.json"));
}

/// The certificate password typed at the interactive prompt is not echoed. The binary runs on a
/// pseudo-terminal (Python `pty`), so the old line-mode prompt would have echoed the typed text back.
#[cfg(unix)]
#[test]
fn certificate_password_prompt_does_not_echo() {
    let py = std::env::var("RV_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
    if !std::process::Command::new(&py)
        .args(["-c", "import pty"])
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!("skipping: no Python with pty ({})", py);
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let script = r#"
import os, pty, sys, time
exe, args = sys.argv[1], sys.argv[1:]
pid, fd = pty.fork()
if pid == 0:
    os.environ["RV_NO_TIMESTAMP"] = "1"
    os.environ.pop("RV_SIGN_PASSWORD", None)
    os.execv(exe, args)
buf = b""
deadline = time.time() + 60
while b"password" not in buf.lower() and time.time() < deadline:
    try:
        buf += os.read(fd, 4096)
    except OSError:
        break
time.sleep(0.5)  # let the prompt switch echo off before typing
os.write(fd, b"typed-secret-123\n")
while True:
    try:
        chunk = os.read(fd, 4096)
    except OSError:
        break
    if not chunk:
        break
    buf += chunk
_, status = os.waitpid(pid, 0)
sys.stdout.write(buf.decode("utf-8", "replace"))
sys.stdout.write("\nEXIT=%d\n" % os.waitstatus_to_exitcode(status))
"#;
    let res = std::process::Command::new(&py)
        .arg("-c")
        .arg(script)
        .arg(assert_cmd::cargo::cargo_bin("rv-pdfauth"))
        .args(["analyze", "-i"])
        .arg(sample())
        .args(["--progress", "none", "--sign-cert"])
        .arg(out.path().join("missing.p12"))
        .arg("-o")
        .arg(out.path())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&res.stdout);
    assert!(text.contains("input hidden"), "no prompt: {}", text);
    assert!(
        !text.contains("typed-secret-123"),
        "the password was echoed: {}",
        text
    );
    assert!(text.contains("EXIT=1"), "{}", text);
}
