//! The worked examples in each command's `--help` (EXAMPLES:) really work: they run, in a temporary folder,
//! against copies of the repository's sample evidence. The sample paths of the examples (D:\evidence,
//! E:\CASE-014, the example run id, the fingerprint originals, ...) are mapped to that folder. Every subcommand
//! that `docs-cli` documents (nested `fingerprints` commands included), plus `docs-cli` itself, is run; the only
//! exceptions are in PARSE_ONLY (they need the network) and are parsed by the unit test of src/main.rs, which also
//! fails when a hidden subcommand is added that this list does not see. The local fingerprint database is kept
//! in the temporary folder (LOCALAPPDATA / XDG_DATA_HOME), never the examiner's own.

use assert_cmd::Command;
use rv_pdfauth_core::analyzer::{DomainOptions, Isolation};
use rv_pdfauth_core::{CASE_ROLE, CaseRole, RunConfig, run};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

const TOOL: &str = "rv-pdfauth";
/// Subcommands whose examples are not run here, with the reason. They are parsed by the unit test in src/main.rs.
const PARSE_ONLY: &[(&str, &str)] = &[("timestamp", "needs an RFC 3161 TSA on the network")];
/// The run id the examples use; replaced by a real run id of the same folder.
const EXAMPLE_RUN: &str = "20261019T101500Z-a1b2c3";
/// `analyze` exits 3 when a high-severity finding is raised: the sample evidence is built to raise some.
const EXIT_FINDINGS: i32 = 3;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bin(cwd: &Path) -> Command {
    let mut c = Command::cargo_bin(TOOL).unwrap();
    let data = cwd.join("local-app-data");
    c.current_dir(cwd)
        .env("RV_NO_TIMESTAMP", "1") // offline: the examples must not depend on a TSA
        .env("RV_NO_UPDATE_CHECK", "1") // update-check answers "turned off" (exit 0) without the network
        .env("LOCALAPPDATA", &data) // the local fingerprint database of this test only (Windows)
        .env("XDG_DATA_HOME", &data); // the same on Linux and macOS
    c
}

fn help(sub: &str) -> String {
    let mut args: Vec<&str> = sub.split(' ').collect();
    args.push("--help");
    let out = Command::cargo_bin(TOOL)
        .unwrap()
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

/// Command lines under EXAMPLES: (same rule as the unit test in src/main.rs).
fn examples(help: &str) -> Vec<String> {
    let Some(section) = help.split("EXAMPLES:\n").nth(1) else {
        return vec![];
    };
    // Commands are indented by 4. clap wraps a long one onto further lines with the same indent: those lines
    // continue the command.
    let prefix = format!("{} ", TOOL);
    let mut out: Vec<String> = vec![];
    let mut in_command = false;
    for l in section
        .lines()
        .take_while(|l| l.is_empty() || l.starts_with(' '))
    {
        let t = l.trim();
        if t.starts_with(&prefix) {
            out.push(t.to_string());
            in_command = true;
        } else if in_command && l.starts_with("    ") && !t.is_empty() {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(t);
            }
        } else {
            in_command = false;
        }
    }
    out
}

fn split_args(line: &str) -> Vec<String> {
    let (mut args, mut cur, mut quoted, mut any) = (vec![], String::new(), false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    args.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        args.push(cur);
    }
    if let Some(i) = args.iter().position(|a| a == ">") {
        args.truncate(i);
    }
    args
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}

/// The first run folder name under `dir` (a logs/ or runs/<tool>/ folder).
fn a_run(dir: &Path) -> String {
    let mut runs: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {}", dir.display(), e))
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    runs.sort();
    runs.into_iter()
        .next()
        .unwrap_or_else(|| panic!("no run in {}", dir.display()))
}

/// Maps the sample paths of an example argument to the temporary folder.
fn localize(arg: &str, t: &Path) -> String {
    let unix = arg.replace('\\', "/");
    for (sample, local) in [
        ("D:/evidence", "D-evidence"),
        ("E:/mailbox-export", "E-mailbox-export"),
    ] {
        if let Some(rest) = unix.strip_prefix(sample) {
            return format!("{}{}", t.join(local).display(), rest);
        }
    }
    if let Some(rest) = unix.strip_prefix("E:/CASE-014") {
        let rest = if rest.contains(EXAMPLE_RUN) {
            let run = a_run(&t.join("CASE-014/runs").join(TOOL));
            rest.replace(EXAMPLE_RUN, &run)
        } else {
            rest.to_string()
        };
        return format!("{}{}", t.join("CASE-014").display(), rest);
    }
    if unix.starts_with("rakvor-out/logs/") && unix.contains(EXAMPLE_RUN) {
        return unix.replace(EXAMPLE_RUN, &a_run(&t.join("rakvor-out/logs")));
    }
    if unix == "digicert-tsa-roots.pem" {
        // Any CA certificate will do: the offline runs have no timestamp token to chain to it.
        return repo()
            .join("packaging/signing/rakvor-code-signing-root.cer")
            .display()
            .to_string();
    }
    arg.to_string()
}

#[test]
fn help_examples_run_against_the_sample_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path();
    let sample = repo().join("examples/sample/evidence");
    copy_dir(&sample, &t.join("D-evidence"));
    copy_dir(&sample, &t.join("E-mailbox-export"));
    std::fs::create_dir_all(t.join("evidence")).unwrap();
    std::fs::copy(sample.join("original.pdf"), t.join("evidence/contract.pdf")).unwrap();
    // Known originals for `fingerprints learn`: copies of the synthetic samples, whose producer strings the
    // examples teach under the example labels.
    std::fs::create_dir_all(t.join("originals")).unwrap();
    for (from, to) in [
        ("online-editor.pdf", "originals/smallpdf-1.pdf"),
        ("online-editor.pdf", "originals/smallpdf-2.pdf"),
        ("online-editor.pdf", "made-with-ilovepdf.pdf"),
        ("original.pdf", "a.pdf"),
        ("original.pdf", "b.pdf"),
    ] {
        std::fs::copy(sample.join(from), t.join(to)).unwrap();
    }
    std::fs::write(
        t.join("files.txt"),
        "# sample list\nevidence/contract.pdf\n",
    )
    .unwrap();
    // The extra fingerprint database of the batch example: the bundled starter database, exported.
    let out = bin(t)
        .args([
            "fingerprints",
            "export",
            "--db",
            "starter",
            "--out",
            "case-014-fingerprints.json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    if CASE_ROLE != CaseRole::Intake {
        // An analysis tool does not register evidence: register the sample in the example case first, through
        // the core with the intake role (what rv-hashinv does), so `--evidence E0001,E0002` exists.
        let cfg = RunConfig {
            inputs: vec![t.join("D-evidence")],
            recursive: true,
            actor: "seed".into(),
            case_id: Some("CASE-014".into()),
            jurisdiction: "fr".into(),
            case_dir: Some(t.join("CASE-014")),
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
        run(&cfg, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    }

    // Every documented subcommand (docs-cli lists them, nested ones under their parent; analyze comes first, so
    // its runs exist before the verify examples), then docs-cli itself (hidden from the list, its example only
    // prints).
    let (top, nested) = (format!("## {} ", TOOL), format!("### {} ", TOOL));
    let docs = bin(t).arg("docs-cli").output().unwrap();
    let mut order: Vec<String> = String::from_utf8_lossy(&docs.stdout)
        .lines()
        .filter_map(|l| {
            l.strip_prefix(&top)
                .or_else(|| l.strip_prefix(&nested))
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        order.first().map(String::as_str),
        Some("analyze"),
        "{:?}",
        order
    );
    assert!(
        order.iter().any(|c| c == "fingerprints learn"),
        "{:?}",
        order
    );
    order.push("docs-cli".into());
    let mut ran = 0;
    for sub in &order {
        if PARSE_ONLY.iter().any(|(p, _)| p == sub) {
            continue;
        }
        let words: Vec<&str> = sub.split(' ').collect();
        let lines = examples(&help(sub));
        assert!(!lines.is_empty(), "`{} {}` has no EXAMPLES", TOOL, sub);
        for line in lines {
            let args = split_args(&line);
            if args.get(1..=words.len()).is_none_or(|w| *w != words[..]) {
                // analyze's help also shows how to verify afterwards; that command is run with its own help.
                continue;
            }
            let local: Vec<String> = args[1..].iter().map(|a| localize(a, t)).collect();
            let out = bin(t).args(&local).output().unwrap();
            let ok = match out.status.code() {
                Some(0) => true,
                Some(EXIT_FINDINGS) => sub == "analyze",
                _ => false,
            };
            assert!(
                ok,
                "example `{}` (run as {:?}) failed with {:?}:\n{}{}",
                line,
                local,
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            ran += 1;
        }
    }
    assert!(ran >= 25, "{} examples ran", ran);
    // The case examples analysed the registered sample and recorded their runs in the case log.
    let case_log = std::fs::read_to_string(t.join("CASE-014/case-log.jsonl")).unwrap();
    assert!(case_log.contains("\"rv-pdfauth\""), "{}", case_log);
    // The learn examples wrote the test's own local database, never the examiner's.
    assert!(
        t.join("lab-fingerprints-2026-10.json").is_file()
            && t.join("lab-fingerprints-2026-11.json").is_file()
    );
    // Every timestamp example names a log folder the examples above created (it is only parsed: no TSA).
    for line in examples(&help("timestamp")) {
        let args = split_args(&line);
        assert!(
            t.join(localize(&args[2], t)).join("audit.jsonl").is_file(),
            "{}",
            line
        );
    }
}
