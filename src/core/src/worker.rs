//! Worker-process isolation for the PDF analysis.
//!
//! The parent (the run, which owns the audit log) starts one child per PDF:
//!
//! ```text
//! <exe> worker --in <evidence> --item-dir <item folder> --opts <DomainOptions as JSON> [--expect-digest <algo>:<hex>]
//! ```
//!
//! Protocol `rv-pdfauth-worker/1`:
//! 1. The child starts with stdin, stdout and stderr as pipes (nothing inherited). The parent applies the
//!    limits — Unix: `RLIMIT_AS` = `worker_memory_mb` and `RLIMIT_CORE` = 0 (no core dump holding evidence)
//!    in `pre_exec`; Windows: a Job Object with `JOB_OBJECT_LIMIT_PROCESS_MEMORY`, kill-on-close and
//!    die-on-unhandled-exception (no error dialog) — then writes `RV-WORKER-GO\n` and closes stdin. The child
//!    waits for that line before it opens the evidence, so no evidence byte is parsed outside the limits.
//! 2. The child runs the same in-process code as `Isolation::InProcess` (`analyzer::analyze_item`), writes
//!    its outputs into the item folder only, and prints exactly one JSON document on stdout:
//!    `{"protocol": "rv-pdfauth-worker/1", "analysis": <Analysis>}`, then exits 0. It never touches the audit
//!    log: the parent hashes and audits every output after the child has ended.
//! 3. The parent enforces `worker_timeout_s` (kill), caps stdout at 64 MiB and keeps the last 4 KiB of stderr.
//!
//! Failure (timeout, crash, memory limit, bad JSON, cannot spawn/contain) → finding `PARSER_FAILED` (high;
//! `reason`, `exit_code`) and `details.worker = {reason, exit_code, stderr_tail, …}`; the bulk run continues.
//!
//! **Crash-output policy:** files a failed worker left in the item folder are **kept and listed** with kind
//! `partial_output`, so the pipeline hashes and audits them like any other output (nothing the tool wrote
//! escapes the chain, and nothing is silently deleted). They come from an interrupted analysis and support
//! no finding. After a successful run, files present but not listed by the child are added as
//! `unlisted_output`, and listed paths that escape the item folder are rejected (recorded, not audited).

use crate::analyzer::{
    Analysis, AnalyzeCtx, DomainOptions, Finding, Isolation, OutputFile, Severity, analyze,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub const PROTOCOL: &str = "rv-pdfauth-worker/1";
pub const GO: &[u8] = b"RV-WORKER-GO\n";
const MAX_STDOUT: u64 = 64 * 1024 * 1024;
const STDERR_TAIL: usize = 4096;

#[derive(Serialize, Deserialize)]
struct Reply {
    protocol: String,
    analysis: Analysis,
}

struct Failure {
    reason: &'static str,
    exit_code: String,
    stderr_tail: String,
    detail: Option<String>,
}

impl Failure {
    fn new(reason: &'static str, detail: impl Into<String>) -> Failure {
        Failure {
            reason,
            exit_code: "-".into(),
            stderr_tail: String::new(),
            detail: Some(detail.into()),
        }
    }
}

/// Timeout actually applied. Debug builds honour `RV_WORKER_TEST_TIMEOUT_S` (tests only).
fn effective_timeout(configured: u64) -> u64 {
    #[cfg(debug_assertions)]
    if let Some(v) = std::env::var("RV_WORKER_TEST_TIMEOUT_S")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        return v.max(1);
    }
    configured.max(1)
}

/// Parent side: analyse `ctx` in a child process of `exe`.
pub fn analyze_in_worker(exe: &Path, ctx: &AnalyzeCtx) -> Analysis {
    let started = Instant::now();
    let timeout = Duration::from_secs(effective_timeout(ctx.opts.worker_timeout_s));
    let mem_mb = ctx.opts.worker_memory_mb;
    let child_opts = DomainOptions {
        isolation: Isolation::InProcess,
        ..ctx.opts.clone()
    };
    let outcome = spawn_and_wait(exe, ctx, &child_opts, timeout, mem_mb);
    let limits = json!({"timeout_s": timeout.as_secs(), "memory_mb": mem_mb,
        "elapsed_ms": started.elapsed().as_millis() as u64, "protocol": PROTOCOL});
    match outcome {
        Ok(a) => accept(a, ctx.item_dir, limits),
        Err(f) => failed(ctx, f, limits),
    }
}

fn describe(status: &ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("signal {}", sig);
        }
    }
    match status.code() {
        // NTSTATUS-style codes (0xC0000005 access violation, 0xC0000409 fail-fast/abort) read better in hex.
        Some(c) if cfg!(windows) && (c as u32) >= 0x8000_0000 => format!("0x{:08X}", c as u32),
        Some(c) => c.to_string(),
        None => "-".into(),
    }
}

fn spawn_and_wait(
    exe: &Path,
    ctx: &AnalyzeCtx,
    child_opts: &DomainOptions,
    timeout: Duration,
    mem_mb: u64,
) -> Result<Analysis, Failure> {
    let opts_json = serde_json::to_string(child_opts)
        .map_err(|e| Failure::new("spawn_failed", e.to_string()))?;
    let mut cmd = Command::new(exe);
    cmd.arg("worker")
        .arg("--in")
        .arg(ctx.path)
        .arg("--item-dir")
        .arg(ctx.item_dir)
        .arg("--opts")
        .arg(&opts_json);
    if let Some((algo, hex)) = ctx.expected_digest {
        cmd.arg("--expect-digest")
            .arg(format!("{}:{}", algo.name(), hex));
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sys::configure(&mut cmd, mem_mb);
    let mut child = cmd.spawn().map_err(|e| {
        Failure::new(
            "spawn_failed",
            format!("cannot start {} worker: {}", exe.display(), e),
        )
    })?;
    // Held until return: on Windows, closing the job handle kills anything left in it.
    let _job = match sys::contain(&child, mem_mb) {
        Ok(g) => g,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::new("isolation_failed", e));
        }
    };
    let mut stdout = child.stdout.take();
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.take(MAX_STDOUT + 1).read_to_end(&mut buf);
            let _ = std::io::copy(out, &mut std::io::sink());
        }
        buf
    });
    let mut stderr = child.stderr.take();
    let err_t = std::thread::spawn(move || {
        let mut tail: Vec<u8> = Vec::new();
        if let Some(err) = stderr.as_mut() {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = err.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > 4 * STDERR_TAIL {
                    tail.drain(..tail.len() - STDERR_TAIL);
                }
            }
        }
        let start = tail.len().saturating_sub(STDERR_TAIL);
        String::from_utf8_lossy(&tail[start..]).into_owned()
    });
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(GO);
        let _ = stdin.flush();
    } // dropped: the child sees EOF after the go line
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                break child.wait().ok();
            }
        }
    };
    let out = out_t.join().unwrap_or_default();
    let stderr_tail = err_t.join().unwrap_or_default();
    let exit_code = status.as_ref().map(describe).unwrap_or_else(|| "-".into());
    let fail = |reason: &'static str, detail: Option<String>| Failure {
        reason,
        exit_code: exit_code.clone(),
        stderr_tail: stderr_tail.clone(),
        detail,
    };
    if timed_out {
        return Err(fail(
            "timeout",
            Some(format!("killed after {} s", timeout.as_secs())),
        ));
    }
    let success = status.as_ref().is_some_and(ExitStatus::success);
    if !success {
        // Rust's allocation-failure handler prints this line before aborting.
        if stderr_tail.contains("memory allocation of") {
            return Err(fail(
                "memory_limit",
                Some(format!("exceeded the {} MB worker limit", mem_mb)),
            ));
        }
        return Err(fail("crashed", None));
    }
    if out.len() as u64 > MAX_STDOUT {
        return Err(fail(
            "bad_output",
            Some(format!("more than {} bytes on stdout", MAX_STDOUT)),
        ));
    }
    let parsed = serde_json::from_slice::<Reply>(&out).or_else(|e| {
        // Tolerate stray lines before the reply (e.g. a library printing to stdout): use the last line.
        out.split(|&b| b == b'\n')
            .rfind(|l| !l.iter().all(u8::is_ascii_whitespace))
            .ok_or(e)
            .and_then(serde_json::from_slice::<Reply>)
    });
    match parsed {
        Ok(r) if r.protocol == PROTOCOL => Ok(r.analysis),
        Ok(r) => Err(fail(
            "bad_output",
            Some(format!("unknown protocol {:?}", r.protocol)),
        )),
        Err(e) => Err(fail(
            "bad_output",
            Some(format!("invalid JSON reply: {}", e)),
        )),
    }
}

/// Every regular file under `dir` (no symlink followed), as sorted forward-slash relative paths.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if md.is_dir() {
                walk(base, &p, out);
            } else if md.is_file() {
                if let Ok(rel) = p.strip_prefix(base) {
                    let parts: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    out.push(parts.join("/"));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn safe_rel(rel: &str) -> bool {
    let p = Path::new(rel);
    !rel.is_empty()
        && !rel.contains('\\')
        && p.components().all(|c| matches!(c, Component::Normal(_)))
}

fn accept(mut a: Analysis, item_dir: &Path, mut worker: Value) -> Analysis {
    let present = files_under(item_dir);
    let mut rejected = Vec::new();
    let mut outputs: Vec<OutputFile> = Vec::new();
    for o in a.outputs.drain(..) {
        if safe_rel(&o.rel_path) && present.contains(&o.rel_path) {
            if !outputs.iter().any(|x| x.rel_path == o.rel_path) {
                outputs.push(o);
            }
        } else {
            rejected.push(json!({"rel_path": o.rel_path, "kind": o.kind}));
        }
    }
    for f in &present {
        if !outputs.iter().any(|o| &o.rel_path == f) {
            outputs.push(OutputFile {
                rel_path: f.clone(),
                kind: "unlisted_output".into(),
            });
        }
    }
    a.outputs = outputs;
    worker["status"] = json!("completed");
    if !rejected.is_empty() {
        worker["rejected_outputs"] = rejected.into();
    }
    if !a.details.is_object() {
        a.details = json!({});
    }
    a.details["worker"] = worker;
    a
}

fn failed(ctx: &AnalyzeCtx, f: Failure, mut worker: Value) -> Analysis {
    let base = analyze(ctx.file_name, ctx.size, ctx.head);
    let mut findings = base.findings.clone();
    findings.push(
        Finding::new("PARSER_FAILED", Severity::High)
            .with("reason", f.reason)
            .with("exit_code", f.exit_code.clone()),
    );
    let partial = files_under(ctx.item_dir);
    worker["status"] = json!("failed");
    worker["reason"] = json!(f.reason);
    worker["exit_code"] = json!(f.exit_code);
    worker["stderr_tail"] = json!(f.stderr_tail);
    if let Some(d) = f.detail {
        worker["detail"] = json!(d);
    }
    worker["partial_outputs"] = json!(partial);
    Analysis {
        detected_type: base.detected_type,
        extension: base.extension,
        findings,
        details: json!({"pdf_analysis": "not_completed", "worker": worker}),
        outputs: partial
            .into_iter()
            .map(|rel_path| OutputFile {
                rel_path,
                kind: "partial_output".into(),
            })
            .collect(),
    }
}

/// Debug builds only: `RV_WORKER_TEST_FAULT=crash|hang|oom` makes the worker misbehave on purpose so the
/// isolation can be tested end to end. Release builds ignore the variable.
#[cfg(debug_assertions)]
fn test_fault(item_dir: &Path, mem_mb: u64, stderr: &mut dyn Write) {
    match std::env::var("RV_WORKER_TEST_FAULT").as_deref() {
        Ok("crash") => {
            let _ = std::fs::write(item_dir.join("partial-before-crash.bin"), b"partial");
            std::process::abort();
        }
        Ok("hang") => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        Ok("oom") => {
            // Reserve more than the limit: fails at once under RLIMIT_AS / the Job Object (no page is touched,
            // so a missing limit cannot eat the examiner's RAM: exit 86 instead).
            let n = ((mem_mb.max(64) + 512) * 1024 * 1024) as usize;
            let mut v: Vec<u8> = Vec::new();
            if v.try_reserve_exact(n).is_err() {
                std::alloc::handle_alloc_error(
                    std::alloc::Layout::from_size_align(n, 1).expect("layout"),
                );
            }
            let _ = writeln!(stderr, "worker test fault: memory limit not enforced");
            std::process::exit(86);
        }
        _ => {}
    }
}

/// Child entry point (`<exe> worker …`). The front-ends call it before parsing their own arguments and exit
/// with its return value. Exit 0 = a reply was written; 2 = bad invocation.
pub fn worker_main() -> i32 {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
    run_child(
        &args,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// The worker with explicit streams (the protocol channel is `stdout`; diagnostics go to `stderr`, whose
/// tail the parent records).
fn run_child(
    args: &[std::ffi::OsString],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let (mut input, mut item_dir, mut opts) = (None, None, None);
    let mut expect: Option<(crate::hash::HashAlgo, String)> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--in") => input = it.next().map(PathBuf::from),
            Some("--item-dir") => item_dir = it.next().map(PathBuf::from),
            Some("--opts") => opts = it.next().and_then(|s| s.to_str()).map(String::from),
            Some("--expect-digest") => {
                let v = it.next().and_then(|s| s.to_str()).unwrap_or_default();
                match v
                    .split_once(':')
                    .and_then(|(a, h)| Some((crate::hash::HashAlgo::parse(a)?, h.to_string())))
                {
                    Some(x) => expect = Some(x),
                    None => {
                        let _ = writeln!(stderr, "rv-pdfauth worker: bad --expect-digest {:?}", v);
                        return 2;
                    }
                }
            }
            _ => {
                let _ = writeln!(stderr, "rv-pdfauth worker: unexpected argument {:?}", a);
                return 2;
            }
        }
    }
    let (Some(input), Some(item_dir), Some(opts)) = (input, item_dir, opts) else {
        let _ = writeln!(
            stderr,
            "rv-pdfauth worker: usage: worker --in <file> --item-dir <dir> --opts <json>"
        );
        return 2;
    };
    let mut opts: DomainOptions = match serde_json::from_str(&opts) {
        Ok(o) => o,
        Err(e) => {
            let _ = writeln!(stderr, "rv-pdfauth worker: bad --opts: {}", e);
            return 2;
        }
    };
    opts.isolation = Isolation::InProcess;
    // Wait for the parent to finish applying the limits.
    let mut go = Vec::new();
    let _ = stdin.take(64).read_until_newline(&mut go);
    if go != GO {
        let _ = writeln!(stderr, "rv-pdfauth worker: no go signal from the parent");
        return 2;
    }
    #[cfg(debug_assertions)]
    test_fault(&item_dir, opts.worker_memory_mb, stderr);

    let (size, head) = match std::fs::File::open(&input).and_then(|mut f| {
        let size = f.metadata()?.len();
        let mut head = Vec::with_capacity(4096);
        (&mut f).take(4096).read_to_end(&mut head)?;
        Ok((size, head))
    }) {
        Ok(x) => x,
        Err(e) => {
            let _ = writeln!(
                stderr,
                "rv-pdfauth worker: cannot open {}: {}",
                input.display(),
                e
            );
            return 2;
        }
    };
    let file_name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let analysis = crate::analyzer::analyze_item(&AnalyzeCtx {
        path: &input,
        file_name: &file_name,
        size,
        head: &head,
        item_dir: &item_dir,
        opts: &opts,
        expected_digest: expect.as_ref().map(|(a, h)| (*a, h.as_str())),
    });
    let reply = Reply {
        protocol: PROTOCOL.into(),
        analysis,
    };
    let ok = serde_json::to_writer(&mut *stdout, &reply).is_ok()
        && writeln!(stdout).is_ok()
        && stdout.flush().is_ok();
    if ok { 0 } else { 2 }
}

trait ReadLine {
    fn read_until_newline(&mut self, buf: &mut Vec<u8>) -> std::io::Result<()>;
}

impl<R: Read> ReadLine for R {
    fn read_until_newline(&mut self, buf: &mut Vec<u8>) -> std::io::Result<()> {
        let mut b = [0u8; 1];
        loop {
            if self.read(&mut b)? == 0 {
                return Ok(());
            }
            buf.push(b[0]);
            if b[0] == b'\n' {
                return Ok(());
            }
        }
    }
}

#[cfg(unix)]
mod sys {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    pub struct Guard;

    pub fn configure(cmd: &mut Command, mem_mb: u64) {
        // SAFETY: only async-signal-safe libc calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                let none = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_CORE, &none);
                if mem_mb > 0 {
                    let bytes = mem_mb.saturating_mul(1024 * 1024) as libc::rlim_t;
                    let lim = libc::rlimit {
                        rlim_cur: bytes,
                        rlim_max: bytes,
                    };
                    if libc::setrlimit(libc::RLIMIT_AS, &lim) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }

    /// Limits are already applied in `pre_exec`.
    pub fn contain(_child: &Child, _mem_mb: u64) -> Result<Guard, String> {
        Ok(Guard)
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    /// Closing the job handle kills the worker if it is still running (KILL_ON_JOB_CLOSE).
    pub struct Guard(HANDLE);

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: handle created by CreateJobObjectW and closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub fn configure(cmd: &mut Command, _mem_mb: u64) {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    fn last_error(what: &str) -> String {
        format!("{} failed: {}", what, std::io::Error::last_os_error())
    }

    pub fn contain(child: &Child, mem_mb: u64) -> Result<Guard, String> {
        // SAFETY: plain Win32 calls on handles we own; the struct is zero-initialised as documented.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(last_error("CreateJobObjectW"));
            }
            let guard = Guard(job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            if mem_mb > 0 {
                info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                info.ProcessMemoryLimit = mem_mb.saturating_mul(1024 * 1024) as usize;
            }
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return Err(last_error("SetInformationJobObject"));
            }
            if AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0 {
                return Err(last_error("AssignProcessToJobObject"));
            }
            Ok(guard)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_must_stay_inside_the_item_folder() {
        assert!(safe_rel("visual/rev-01-vs-02-page-001.png"));
        assert!(!safe_rel("../escape.bin"));
        assert!(!safe_rel("/etc/passwd"));
        assert!(!safe_rel("a\\..\\b"));
        assert!(!safe_rel(""));
    }

    #[test]
    fn accept_adds_unlisted_files_and_rejects_escapes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("visual")).unwrap();
        std::fs::write(dir.path().join("visual/a.png"), b"x").unwrap();
        std::fs::write(dir.path().join("stray.bin"), b"y").unwrap();
        let a = Analysis {
            outputs: vec![
                OutputFile {
                    rel_path: "visual/a.png".into(),
                    kind: "visual_diff".into(),
                },
                OutputFile {
                    rel_path: "../../outside".into(),
                    kind: "revision".into(),
                },
            ],
            ..Analysis::default()
        };
        let a = accept(a, dir.path(), json!({}));
        let got: Vec<(&str, &str)> = a
            .outputs
            .iter()
            .map(|o| (o.rel_path.as_str(), o.kind.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("visual/a.png", "visual_diff"),
                ("stray.bin", "unlisted_output")
            ]
        );
        assert_eq!(
            a.details["worker"]["rejected_outputs"][0]["rel_path"],
            "../../outside"
        );
        assert_eq!(a.details["worker"]["status"], "completed");
    }

    #[test]
    fn missing_executable_is_a_parser_failure_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let ev = dir.path().join("x.pdf");
        std::fs::write(&ev, b"%PDF-1.7\n").unwrap();
        let item = dir.path().join("item");
        std::fs::create_dir_all(&item).unwrap();
        let opts = DomainOptions::default();
        let a = analyze_in_worker(
            Path::new("/nonexistent/rv-pdfauth"),
            &AnalyzeCtx {
                path: &ev,
                file_name: "x.pdf",
                size: 9,
                head: b"%PDF-1.7\n",
                item_dir: &item,
                opts: &opts,
                expected_digest: None,
            },
        );
        let f = a
            .findings
            .iter()
            .find(|f| f.code == "PARSER_FAILED")
            .unwrap();
        assert_eq!(f.severity, Severity::High);
        assert_eq!(f.params["reason"], "spawn_failed");
        assert_eq!(a.details["worker"]["reason"], "spawn_failed");
        assert_eq!(a.detected_type.as_deref(), Some("PDF document"));
    }

    #[test]
    fn child_needs_the_go_line_and_replies_with_one_json_document() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/visual/one-revision.pdf");
        let dir = tempfile::tempdir().unwrap();
        let opts = serde_json::to_string(&DomainOptions {
            visual_diff: false,
            ..DomainOptions::default()
        })
        .unwrap();
        let args: Vec<std::ffi::OsString> = vec![
            "--in".into(),
            fixture.into(),
            "--item-dir".into(),
            dir.path().into(),
            "--opts".into(),
            opts.into(),
        ];
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run_child(&args, &mut &b""[..], &mut out, &mut err), 2);
        assert!(String::from_utf8_lossy(&err).contains("no go signal"));
        assert!(out.is_empty());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_child(&args, &mut &GO[..], &mut out, &mut err),
            0,
            "{}",
            String::from_utf8_lossy(&err)
        );
        let reply: Reply = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply.protocol, PROTOCOL);
        assert_eq!(
            reply.analysis.detected_type.as_deref(),
            Some("PDF document")
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            run_child(&["--bogus".into()], &mut &GO[..], &mut out, &mut err),
            2
        );
    }

    #[test]
    fn worker_rehashes_the_mapped_evidence_against_the_first_pass() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/visual/one-revision.pdf");
        let opts = serde_json::to_string(&DomainOptions {
            visual_diff: false,
            ..DomainOptions::default()
        })
        .unwrap();
        let run = |expect: &str| {
            let dir = tempfile::tempdir().unwrap();
            let args: Vec<std::ffi::OsString> = vec![
                "--in".into(),
                fixture.clone().into(),
                "--item-dir".into(),
                dir.path().into(),
                "--opts".into(),
                opts.clone().into(),
                "--expect-digest".into(),
                expect.into(),
            ];
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let rc = run_child(&args, &mut &GO[..], &mut out, &mut err);
            (rc, out, String::from_utf8_lossy(&err).into_owned())
        };
        let good = crate::hash::hash_bytes(
            &std::fs::read(&fixture).unwrap(),
            crate::hash::HashAlgo::Blake3,
        );
        let (rc, out, err) = run(&format!("blake3:{}", good));
        assert_eq!(rc, 0, "{}", err);
        let reply: Reply = serde_json::from_slice(&out).unwrap();
        assert_eq!(reply.analysis.details["evidence_rehash"]["match"], true);
        assert_eq!(
            reply.analysis.details["evidence_rehash"]["algorithm"],
            "blake3"
        );
        let (rc, out, _) = run(&format!("blake3:{}", "ab".repeat(32)));
        assert_eq!(rc, 0);
        let reply: Reply = serde_json::from_slice(&out).unwrap();
        assert!(
            reply
                .analysis
                .findings
                .iter()
                .any(|f| f.code == "EVIDENCE_CHANGED_DURING_ANALYSIS")
        );
        let (rc, _, err) = run("crc32:00");
        assert_eq!(rc, 2);
        assert!(err.contains("--expect-digest"));
    }

    #[test]
    fn options_round_trip_as_json() {
        let o = DomainOptions {
            dpi: 72,
            fingerprints_db: Some(PathBuf::from("/x/db.json")),
            isolation: Isolation::Subprocess {
                exe: PathBuf::from("/bin/rv"),
            },
            ..DomainOptions::default()
        };
        let back: DomainOptions =
            serde_json::from_str(&serde_json::to_string(&o).unwrap()).unwrap();
        assert_eq!(back, o);
        let partial: DomainOptions = serde_json::from_str(r#"{"dpi": 90}"#).unwrap();
        assert_eq!(partial.dpi, 90);
        assert_eq!(partial.max_pages, DomainOptions::default().max_pages);
    }
}
