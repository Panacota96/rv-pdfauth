//! Intake by RV HashInv (`--intake hashinv`): before analysing, rv-pdfauth runs the Rakvor intake tool
//! (`rv-hashinv scan … --case-dir`) on the same evidence, so one action gives the hash inventory, the
//! evidence registry and the PDF analysis in one case folder.
//!
//! rv-pdfauth does not hash or register evidence on rv-hashinv's behalf: the hash report and the registry
//! are produced by rv-hashinv itself (its own executable, tests and audit log). rv-pdfauth only
//! 1. finds the executable and checks its version (≥ [`MIN_HASHINV`]),
//! 2. records the executable's path, version and SHA-256 and the exact arguments in its own audit log,
//! 3. runs it with `--progress jsonl`, forwards its progress, and stops if it fails,
//! 4. then analyses the items, which are verified against the registry rv-hashinv just wrote.

use crate::events::Event;
use crate::hash::{HashAlgo, hash_reader};
use crate::run::RunConfig;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Oldest rv-hashinv that has the case folder (`--case-dir`, evidence registry).
pub const MIN_HASHINV: (u64, u64, u64) = (0, 3, 0);
/// Environment variable that may point at the rv-hashinv executable.
pub const ENV_HASHINV: &str = "RV_HASHINV";
/// Environment variable used to hand the certificate password to rv-hashinv (child process only).
const ENV_CHILD_PASS: &str = "RV_PDFAUTH_INTAKE_SIGN_PASS";

#[cfg(windows)]
const EXE: &str = "rv-hashinv.exe";
#[cfg(not(windows))]
const EXE: &str = "rv-hashinv";

/// A located and checked rv-hashinv executable.
#[derive(Clone, Debug, PartialEq)]
pub struct IntakeTool {
    pub path: PathBuf,
    pub version: String,
    pub sha256: String,
    /// How it was found: option, env, next_to_rv_pdfauth, install_dir, path.
    pub found_by: String,
}

/// What the intake run produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IntakeOutcome {
    pub run_id: String,
    pub exit_code: i32,
    pub items: u64,
    pub errors: u64,
    pub report_dir: Option<String>,
}

/// Places searched, in order. `explicit` is `--hashinv-exe` / the GUI field.
pub fn candidates(explicit: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    if let Some(p) = explicit {
        out.push((p.to_path_buf(), "option"));
        return out; // an explicit choice is never replaced by another copy
    }
    if let Some(p) = std::env::var_os(ENV_HASHINV).filter(|v| !v.is_empty()) {
        out.push((PathBuf::from(p), "env"));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        out.push((dir.join(EXE), "next_to_rv_pdfauth"));
    }
    // Per-user install location of the RV HashInv installer (packaging/windows/installer.iss).
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push((
            PathBuf::from(local)
                .join("Programs")
                .join("Rakvor")
                .join("RV HashInv")
                .join(EXE),
            "install_dir",
        ));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            out.push((dir.join(EXE), "path"));
        }
    }
    out
}

/// Finds rv-hashinv and checks its version. Error text lists where it looked.
pub fn locate(explicit: Option<&Path>) -> Result<IntakeTool, String> {
    let mut tried = Vec::new();
    for (path, how) in candidates(explicit) {
        if !path.is_file() {
            tried.push(path.display().to_string());
            continue;
        }
        return check(&path, how);
    }
    Err(format!(
        "rv-hashinv not found (install RV HashInv ≥ {}, or give its path with --hashinv-exe or {}). Looked in: {}",
        fmt_ver(MIN_HASHINV),
        ENV_HASHINV,
        tried.join("; ")
    ))
}

/// Runs `<path> --version`, requires "rv-hashinv X.Y.Z" with X.Y.Z ≥ MIN_HASHINV, and hashes the executable.
pub fn check(path: &Path, found_by: &str) -> Result<IntakeTool, String> {
    let out = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", path.display()))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let version = parse_version_line(&text).ok_or_else(|| {
        format!(
            "{} is not rv-hashinv (its --version printed {:?})",
            path.display(),
            text.trim()
        )
    })?;
    if parse_semver(&version).is_none_or(|v| v < MIN_HASHINV) {
        return Err(format!(
            "{} is rv-hashinv {version}; the case folder needs rv-hashinv ≥ {}",
            path.display(),
            fmt_ver(MIN_HASHINV)
        ));
    }
    let f =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let outcome = hash_reader(f, &[HashAlgo::Sha256], |_| true)
        .map_err(|e| format!("cannot hash {}: {e}", path.display()))?
        .ok_or("hashing rv-hashinv was interrupted")?;
    Ok(IntakeTool {
        path: std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
        version,
        sha256: outcome.digests[0].1.clone(),
        found_by: found_by.into(),
    })
}

/// "rv-hashinv 0.3.1" → "0.3.1".
pub fn parse_version_line(s: &str) -> Option<String> {
    let line = s.lines().next()?.trim();
    let v = line.strip_prefix("rv-hashinv ")?.trim();
    parse_semver(v).map(|_| v.to_string())
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

fn fmt_ver(v: (u64, u64, u64)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

/// The rv-hashinv command line for this run (without the executable). The password never appears:
/// it travels in an environment variable of the child process only.
pub fn arguments(cfg: &RunConfig, case_dir: &Path) -> Vec<String> {
    let mut a: Vec<String> = vec!["scan".into()];
    for i in &cfg.inputs {
        a.push("--input".into());
        a.push(i.display().to_string());
    }
    if let Some(l) = &cfg.input_list {
        a.push("--input-list".into());
        a.push(l.display().to_string());
    }
    if cfg.recursive {
        a.push("--recursive".into());
    }
    a.push("--hash".into());
    a.push(
        cfg.algorithms
            .iter()
            .map(|h| h.name())
            .collect::<Vec<_>>()
            .join(","),
    );
    a.push("--lang".into());
    a.push(cfg.langs.join(","));
    a.push("--actor".into());
    a.push(cfg.actor.clone());
    // Only when the case is being created: afterwards case.json decides (both tools agree on this rule).
    if !case_dir.join("case.json").is_file() {
        if let Some(id) = &cfg.case_id {
            a.push("--case".into());
            a.push(id.clone());
        }
        a.push("--jurisdiction".into());
        a.push(cfg.jurisdiction.clone());
    } else if let Some(id) = &cfg.case_id {
        a.push("--case".into());
        a.push(id.clone());
    }
    match &cfg.tsa_url {
        Some(u) => {
            a.push("--tsa-url".into());
            a.push(u.clone());
        }
        None => a.push("--no-timestamp".into()),
    }
    if let Some(c) = &cfg.sign_cert {
        a.push("--sign-cert".into());
        a.push(c.display().to_string());
        a.push("--sign-pass-env".into());
        a.push(ENV_CHILD_PASS.into());
    }
    a.push("--case-dir".into());
    a.push(case_dir.display().to_string());
    a.push("--progress".into());
    a.push("jsonl".into());
    a
}

/// Audit details of the delegated intake (written to rv-pdfauth's audit log before analysis).
pub fn audit_details(tool: &IntakeTool, args: &[String], outcome: &IntakeOutcome) -> Value {
    json!({
        "tool": "rv-hashinv",
        "path": tool.path.display().to_string(),
        "version": tool.version,
        "sha256": tool.sha256,
        "found_by": tool.found_by,
        "arguments": args,
        "exit_code": outcome.exit_code,
        "run_id": outcome.run_id,
        "items": outcome.items,
        "errors": outcome.errors,
        "report_dir": outcome.report_dir,
    })
}

/// Runs the intake. Exit 0 (nothing to report) and 3 (finished, found anomalies such as a renamed
/// executable) both count as done: the registry is complete. Anything else stops the run.
pub fn run_intake(
    tool: &IntakeTool,
    cfg: &RunConfig,
    case_dir: &Path,
    sink: &mut dyn FnMut(&Event),
    cancel: &AtomicBool,
) -> Result<(Vec<String>, IntakeOutcome), String> {
    let args = arguments(cfg, case_dir);
    sink(&Event::IntakeStarted {
        tool: "rv-hashinv".into(),
        version: tool.version.clone(),
        path: tool.path.display().to_string(),
        sha256: tool.sha256.clone(),
    });
    let mut cmd = Command::new(&tool.path);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("RV_NO_TIMESTAMP"); // rv-pdfauth's own choice (--tsa-url / --no-timestamp) is passed explicitly
    if let Some(p) = &cfg.sign_password {
        cmd.env(ENV_CHILD_PASS, p);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flashes up behind the GUI
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", tool.path.display()))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    // stderr is drained on its own thread so a chatty child can never block on a full pipe.
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut s);
        s
    });
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let out_thread = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut outcome = IntakeOutcome::default();
    let mut cancelled = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => forward(&line, &mut outcome, sink),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if cancel.load(Ordering::SeqCst) && !cancelled {
            cancelled = true;
            let _ = child.kill();
        }
    }
    let _ = out_thread.join();
    let status = child
        .wait()
        .map_err(|e| format!("rv-hashinv did not finish: {e}"))?;
    let stderr_text = err_thread.join().unwrap_or_default();
    outcome.exit_code = status.code().unwrap_or(if cancelled { 130 } else { -1 });
    sink(&Event::IntakeFinished {
        run_id: outcome.run_id.clone(),
        exit_code: outcome.exit_code,
        items: outcome.items,
        errors: outcome.errors,
        report_dir: outcome.report_dir.clone(),
    });
    if cancelled {
        return Err("cancelled during the rv-hashinv intake; nothing was analysed".into());
    }
    match outcome.exit_code {
        0 | 3 => Ok((args, outcome)),
        code => {
            let tail: Vec<&str> = stderr_text.lines().rev().take(5).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            Err(format!(
                "rv-hashinv intake failed (exit code {code}); nothing was analysed. {}",
                tail.join(" | ")
            ))
        }
    }
}

/// One JSON line from rv-hashinv → progress events of ours, and the run summary.
fn forward(line: &str, outcome: &mut IntakeOutcome, sink: &mut dyn FnMut(&Event)) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    match v.get("event").and_then(Value::as_str) {
        Some("run_started") => {
            outcome.run_id = v["run_id"].as_str().unwrap_or_default().to_string();
        }
        Some("progress") => sink(&Event::IntakeProgress {
            done: n("done"),
            total: n("total"),
            elapsed_ms: n("elapsed_ms"),
        }),
        Some("run_finished") => {
            outcome.items = n("items");
            outcome.errors = n("errors");
            outcome.report_dir = v["report_dir"].as_str().map(str::to_string);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line() {
        assert_eq!(
            parse_version_line("rv-hashinv 0.3.1\n").as_deref(),
            Some("0.3.1")
        );
        assert_eq!(
            parse_version_line("rv-hashinv 1.0.0-rc.1").as_deref(),
            Some("1.0.0-rc.1")
        );
        assert_eq!(parse_version_line("rv-pdfauth 0.3.1"), None);
        assert_eq!(parse_version_line("rv-hashinv x"), None);
        assert!(parse_semver("0.2.9").unwrap() < MIN_HASHINV);
        assert!(parse_semver("0.3.0").unwrap() >= MIN_HASHINV);
    }

    #[test]
    fn arguments_follow_the_run_and_hide_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RunConfig {
            inputs: vec![PathBuf::from("ev")],
            recursive: true,
            algorithms: vec![HashAlgo::Sha256, HashAlgo::Md5],
            langs: vec!["es".into(), "fr".into()],
            case_id: Some("CASE-1".into()),
            jurisdiction: "fr".into(),
            tsa_url: None,
            sign_cert: Some(PathBuf::from("me.p12")),
            sign_password: Some("s3cret".into()),
            ..RunConfig::default()
        };
        let a = arguments(&cfg, dir.path()).join(" ");
        assert!(
            a.starts_with("scan --input ev --recursive --hash sha256,md5 --lang es,fr"),
            "{a}"
        );
        assert!(
            a.contains("--case CASE-1 --jurisdiction fr --no-timestamp"),
            "{a}"
        );
        assert!(
            a.contains("--sign-pass-env RV_PDFAUTH_INTAKE_SIGN_PASS"),
            "{a}"
        );
        assert!(a.ends_with("--progress jsonl"), "{a}");
        assert!(!a.contains("s3cret"));
        // Existing case: the jurisdiction comes from case.json, so it is not passed again.
        std::fs::write(dir.path().join("case.json"), "{}").unwrap();
        let a = arguments(&cfg, dir.path()).join(" ");
        assert!(!a.contains("--jurisdiction"), "{a}");
    }

    #[test]
    fn explicit_path_is_the_only_candidate() {
        let c = candidates(Some(Path::new("/x/rv-hashinv")));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1, "option");
        assert!(locate(Some(Path::new("/definitely/missing/rv-hashinv"))).is_err());
    }
}
