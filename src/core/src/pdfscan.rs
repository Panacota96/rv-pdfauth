//! In-process PDF analysis of one item: memory-map the evidence read-only, run the PDF layer, then write the
//! derived files (revisions, object dump, embedded streams, visual diff) into the item folder and list them
//! so `run.rs` hashes and audits every one. A worker process runs exactly this code (see `worker.rs`).

use crate::analyzer::{Analysis, AnalyzeCtx, Finding, OutputFile, Severity, analyze};
use rv_pdfauth_pdf::{PdfFinding, PdfOptions, PdfReport, Revision};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::Path;

/// Inflated embedded streams larger than this are not written (decompression bombs).
pub const MAX_INFLATED_BYTES: u64 = 256 * 1024 * 1024;
/// At most this many embedded streams are written per item; the rest are listed as skipped.
pub const MAX_EMBEDDED_OUTPUTS: usize = 1000;

/// `%PDF-` at offset 0, or a `.pdf` name (a PDF with junk before its header is still parsed).
pub fn is_pdf_candidate(file_name: &str, head: &[u8]) -> bool {
    head.starts_with(b"%PDF-")
        || Path::new(file_name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

pub fn map_severity(s: rv_pdfauth_pdf::Severity) -> Severity {
    match s {
        rv_pdfauth_pdf::Severity::Info => Severity::Info,
        rv_pdfauth_pdf::Severity::Low => Severity::Low,
        rv_pdfauth_pdf::Severity::Medium => Severity::Medium,
        rv_pdfauth_pdf::Severity::High => Severity::High,
    }
}

pub fn map_finding(f: &PdfFinding) -> Finding {
    Finding {
        code: f.code.clone(),
        severity: map_severity(f.severity),
        params: f.params.clone(),
    }
}

/// Revisions from the `%%EOF` markers (each revision ends after its `%%EOF` and end-of-line). Used only when
/// the PDF layer reported none, and labelled `revision_source: "eof_scan"` in the details.
pub fn eof_revisions(bytes: &[u8]) -> Vec<Revision> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while let Some(pos) = find(&bytes[i..], b"%%EOF") {
        let mut end = i + pos + 5;
        if bytes.get(end) == Some(&b'\r') {
            end += 1;
        }
        if bytes.get(end) == Some(&b'\n') {
            end += 1;
        }
        out.push(Revision {
            index: out.len() + 1,
            start_offset: start as u64,
            end_offset: end as u64,
            xref_kind: "unknown".into(),
            ..Revision::default()
        });
        start = end;
        i = end;
    }
    out
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Item-folder relative name: alphanumerics, '.', '-', '_' kept, others → '_', at most 60 characters.
pub fn sanitise(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    let s = s.trim_start_matches('.').to_string();
    if s.is_empty() { "unnamed".into() } else { s }
}

/// Writes a new file under the item folder (never overwrites) and lists it.
pub(crate) struct Writer<'a> {
    pub dir: &'a Path,
    pub outputs: Vec<OutputFile>,
    pub errors: Vec<Value>,
}

impl Writer<'_> {
    pub fn write(&mut self, rel: &str, kind: &str, data: &[u8]) -> bool {
        let p = self.dir.join(rel);
        let res = p
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::File::create_new(&p))
            .and_then(|mut f| f.write_all(data).and_then(|_| f.sync_all()));
        match res {
            Ok(()) => {
                self.outputs.push(OutputFile {
                    rel_path: rel.into(),
                    kind: kind.into(),
                });
                true
            }
            Err(e) => {
                // A half-written file is still listed (and therefore hashed and audited), once.
                if p.is_file() && !self.outputs.iter().any(|o| o.rel_path == rel) {
                    self.outputs.push(OutputFile {
                        rel_path: rel.into(),
                        kind: kind.into(),
                    });
                }
                self.errors
                    .push(json!({"file": rel, "kind": kind, "error": e.to_string()}));
                false
            }
        }
    }
}

/// Read-only view of the evidence: memory-mapped (`File::open` = read-only handle), or empty for 0 bytes.
enum Bytes {
    Map(memmap2::Mmap),
    Empty,
}

impl std::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Map(m) => m,
            Bytes::Empty => &[],
        }
    }
}

fn map_read_only(path: &Path) -> std::io::Result<Bytes> {
    let f = std::fs::File::open(path)?;
    if f.metadata()?.len() == 0 {
        return Ok(Bytes::Empty);
    }
    // SAFETY: the map is read-only and private to this process. Evidence must not change while it is examined
    // (work on a write-protected copy); a concurrent truncation could fault, which the worker isolation contains.
    let m = unsafe { memmap2::MmapOptions::new().map(&f)? };
    Ok(Bytes::Map(m))
}

/// The PDF analysis of one item, in this process.
pub fn analyze_pdf(ctx: &AnalyzeCtx) -> Analysis {
    let base = analyze(ctx.file_name, ctx.size, ctx.head);
    let mut findings = base.findings.clone();
    let bytes = match map_read_only(ctx.path) {
        Ok(b) => b,
        Err(e) => {
            findings.push(
                Finding::new("PARSER_FAILED", Severity::High)
                    .with("reason", "unreadable")
                    .with("exit_code", "-"),
            );
            return Analysis {
                findings,
                details: json!({"pdf_analysis": "not_completed", "error": format!("cannot map the file read-only: {}", e)}),
                ..base
            };
        }
    };
    let fp = crate::fingerprints::load(ctx.opts.fingerprints_db.as_deref());
    let pdf_opts = PdfOptions {
        fingerprints: fp.dbs,
        object_tree: ctx.opts.objects_dump,
        embedded: ctx.opts.extract_embedded,
    };
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rv_pdfauth_pdf::analyze(&bytes, &pdf_opts)
    }));
    let mut report: PdfReport = match parsed {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            findings.push(
                Finding::new("PARSER_FAILED", Severity::High)
                    .with("reason", "panic")
                    .with("exit_code", "-"),
            );
            return Analysis {
                findings,
                details: json!({"pdf_analysis": "not_completed", "error": format!("PDF parser panicked: {}", msg),
                    "fingerprint_dbs": fp.details}),
                ..base
            };
        }
    };
    let mut revision_source = "parser";
    if report.revisions.is_empty() {
        let scanned = eof_revisions(&bytes);
        if !scanned.is_empty() {
            report.revisions = scanned;
            revision_source = "eof_scan";
        }
    }
    findings.extend(report.findings.iter().map(map_finding));

    let mut w = Writer {
        dir: ctx.item_dir,
        outputs: vec![],
        errors: vec![],
    };

    // Revisions: exact byte prefixes of the evidence, so each one's digest can be re-derived by anyone.
    let nrev = report.revisions.len();
    if nrev > 1 && ctx.opts.extract_revisions {
        for r in 1..=nrev {
            if let Some(b) = rv_pdfauth_pdf::revision_bytes(&bytes, &report, r) {
                w.write(&format!("revisions/rev-{:02}.pdf", r), "revision", b);
            }
        }
    }

    // Full object tree goes to objects.json, not into the details (it can be large).
    let tree = std::mem::take(&mut report.object_tree);
    if ctx.opts.objects_dump {
        let tree = if tree.is_null() {
            json!({"revisions": []})
        } else {
            tree
        };
        let data = serde_json::to_vec_pretty(&tree).unwrap_or_default();
        w.write("objects.json", "objects_dump", &data);
    }

    let embedded = if ctx.opts.extract_embedded {
        write_embedded(&bytes, &report, &mut w)
    } else {
        Value::Null
    };

    let visual = if ctx.opts.visual_diff && nrev > 1 {
        let v = crate::visual::visual_diff(&bytes, &report, &mut w, ctx.opts);
        findings.extend(v.findings);
        v.details
    } else {
        json!({"status": "not_applicable", "reason": if nrev > 1 { "visual diff disabled" } else { "single revision" }})
    };

    let mut details = serde_json::to_value(&report).unwrap_or(Value::Null);
    if let Some(m) = details.as_object_mut() {
        m.remove("object_tree");
        m.insert("revision_source".into(), revision_source.into());
        m.insert("fingerprint_dbs".into(), fp.details.into());
        m.insert("visual".into(), visual);
        if !embedded.is_null() {
            m.insert("embedded_outputs".into(), embedded);
        }
        if !w.errors.is_empty() {
            m.insert("output_errors".into(), w.errors.clone().into());
        }
    }
    Analysis {
        detected_type: base.detected_type,
        extension: base.extension,
        findings,
        details,
        outputs: w.outputs,
    }
}

/// embedded/<NN>-<name>.<jpg|bin>: DCTDecode streams raw as .jpg, others raw .bin, plus the FlateDecode
/// layer inflated (`.inflated.<jpg|bin>`) when it decodes within [`MAX_INFLATED_BYTES`].
fn write_embedded(bytes: &[u8], report: &PdfReport, w: &mut Writer) -> Value {
    let mut list = Vec::new();
    for (i, e) in report.embedded.iter().enumerate() {
        let n = i + 1;
        let mut entry = json!({"index": n, "object": e.object, "name": e.name, "kind": e.kind,
            "offset": e.offset, "length": e.length, "filters": e.filters});
        if i >= MAX_EMBEDDED_OUTPUTS {
            entry["status"] = json!("skipped: output cap reached");
            list.push(entry);
            continue;
        }
        let kind = if e.kind == "image" {
            "embedded_image"
        } else {
            "embedded_file"
        };
        let raw = usize::try_from(e.offset)
            .ok()
            .zip(usize::try_from(e.offset.saturating_add(e.length)).ok())
            .and_then(|(a, b)| bytes.get(a..b));
        let Some(raw) = raw else {
            entry["status"] = json!("skipped: byte range outside the file");
            list.push(entry);
            continue;
        };
        let stem = format!("embedded/{:02}-{}", n, sanitise(&e.name));
        let only_dct = e.filters.len() == 1 && e.filters[0] == "DCTDecode";
        let raw_rel = format!("{}.{}", stem, if only_dct { "jpg" } else { "bin" });
        if w.write(&raw_rel, kind, raw) {
            entry["file"] = json!(raw_rel);
        }
        if e.filters.first().map(String::as_str) == Some("FlateDecode") {
            let rest_dct = e.filters.len() == 2 && e.filters[1] == "DCTDecode";
            let mut out = Vec::new();
            let res = flate2::read::ZlibDecoder::new(raw)
                .take(MAX_INFLATED_BYTES + 1)
                .read_to_end(&mut out);
            match res {
                Ok(_) if out.len() as u64 > MAX_INFLATED_BYTES => {
                    entry["inflated"] =
                        json!(format!("skipped: larger than {} bytes", MAX_INFLATED_BYTES));
                }
                Ok(_) => {
                    let rel = format!("{}.inflated.{}", stem, if rest_dct { "jpg" } else { "bin" });
                    if w.write(&rel, kind, &out) {
                        entry["inflated"] = json!(rel);
                    }
                }
                Err(err) => entry["inflated"] = json!(format!("failed: {}", err)),
            }
        }
        list.push(entry);
    }
    Value::Array(list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::DomainOptions;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/visual")
            .join(name)
    }

    fn run(path: &Path, dir: &Path, opts: &DomainOptions) -> Analysis {
        let data = std::fs::read(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        crate::analyzer::analyze_item(&AnalyzeCtx {
            path,
            file_name: &name,
            size: data.len() as u64,
            head: &data[..data.len().min(4096)],
            item_dir: dir,
            opts,
        })
    }

    fn pdfium_available() -> bool {
        let env = std::env::var_os("RV_PDFIUM_PATH").map(std::path::PathBuf::from);
        let exe = std::env::current_exe().ok();
        rv_pdfauth_render::library_candidates(env.as_deref(), exe.as_deref())
            .iter()
            .any(|p| p.is_file())
    }

    #[test]
    fn eof_scan_splits_revisions_after_the_end_of_line() {
        let b = b"%PDF-1.4\n1\n%%EOF\r\n2\n%%EOF\n3\n%%EOF";
        let r = eof_revisions(b);
        assert_eq!(r.len(), 3);
        assert_eq!(&b[..r[0].end_offset as usize], b"%PDF-1.4\n1\n%%EOF\r\n");
        assert_eq!(r[1].start_offset, r[0].end_offset);
        assert_eq!(r[2].end_offset as usize, b.len());
        assert!(eof_revisions(b"%PDF-1.4 no marker").is_empty());
    }

    #[test]
    fn candidates_and_names() {
        assert!(is_pdf_candidate("x.bin", b"%PDF-1.7"));
        assert!(is_pdf_candidate("Report.PDF", b"junk"));
        assert!(!is_pdf_candidate("a.txt", b"hello"));
        assert_eq!(sanitise("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitise(""), "unnamed");
        assert_eq!(sanitise("factura 01/é.jpg"), "factura_01_é.jpg");
    }

    #[test]
    fn two_revision_fixture_writes_revisions_objects_and_visual_diff() {
        let src = fixture("two-revisions.pdf");
        let whole = std::fs::read(&src).unwrap();
        let first = std::fs::read(fixture("one-revision.pdf")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let a = run(&src, dir.path(), &DomainOptions::default());
        let kinds: Vec<(&str, &str)> = a
            .outputs
            .iter()
            .map(|o| (o.rel_path.as_str(), o.kind.as_str()))
            .collect();
        assert!(
            kinds.contains(&("revisions/rev-01.pdf", "revision")),
            "{kinds:?}"
        );
        assert!(kinds.contains(&("revisions/rev-02.pdf", "revision")));
        assert!(kinds.contains(&("objects.json", "objects_dump")));
        // Byte-identical to the file prefixes.
        let r1 = std::fs::read(dir.path().join("revisions/rev-01.pdf")).unwrap();
        let r2 = std::fs::read(dir.path().join("revisions/rev-02.pdf")).unwrap();
        assert_eq!(r1, first);
        assert_eq!(r2, whole);
        assert!(whole.starts_with(&r1));
        let objects: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("objects.json")).unwrap())
                .unwrap();
        assert!(objects.is_object());
        assert!(a.details.get("object_tree").is_none());
        assert_eq!(a.details["fingerprint_dbs"][1]["role"], "local");
        assert_eq!(a.detected_type.as_deref(), Some("PDF document"));
        let vis = &a.details["visual"];
        if !pdfium_available() {
            eprintln!("skipping the visual part: PDFium not found (set RV_PDFIUM_PATH)");
            assert_eq!(vis["status"], "skipped");
            assert!(a.findings.iter().any(|f| f.code == "VISUAL_DIFF_SKIPPED"));
            return;
        }
        assert_eq!(vis["status"], "done", "{vis}");
        let pair = &vis["pairs"][0];
        assert_eq!(pair["from_rev"], 1);
        assert_eq!(pair["to_rev"], 2);
        assert_eq!(pair["pages_from"], 1);
        assert_eq!(pair["pages_to"], 2);
        assert_eq!(pair["pages_added"], json!([2]));
        let page = &pair["pages"][0];
        assert_eq!(page["page"], 1);
        assert_eq!(page["changed"], true);
        let png = page["png"].as_str().unwrap();
        assert_eq!(png, "visual/rev-01-vs-02-page-001.png");
        assert!(kinds.contains(&(png, "visual_diff")));
        let bytes = std::fs::read(dir.path().join(png)).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let f = a
            .findings
            .iter()
            .find(|f| f.code == "VISUAL_CHANGE_BETWEEN_REVISIONS")
            .expect("visual finding");
        assert_eq!(f.severity, Severity::Medium);
        assert_eq!(f.params["from_rev"], "1");
        assert_eq!(f.params["to_rev"], "2");
        assert_eq!(f.params["pages"], "1");
        assert_eq!(f.params["pages_added"], "2");
        assert!(f.params["changed_ratio_max"].starts_with("0.0"));
        // Renderer identity for the audit trail.
        assert_eq!(vis["renderer"]["sha256"].as_str().unwrap().len(), 64);
        let s = crate::analyzer::audit_summary(&a.details);
        assert_eq!(s["visual"]["status"], "done");
        assert!(s["fingerprint_dbs"].is_array());
    }

    #[test]
    fn single_revision_writes_no_revision_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = run(
            &fixture("one-revision.pdf"),
            dir.path(),
            &DomainOptions::default(),
        );
        assert!(a.outputs.iter().all(|o| o.kind != "revision"));
        assert_eq!(a.details["visual"]["status"], "not_applicable");
        assert!(a.findings.iter().all(|f| !f.code.starts_with("VISUAL_")));
    }

    #[test]
    fn options_switch_outputs_off() {
        let dir = tempfile::tempdir().unwrap();
        let opts = DomainOptions {
            extract_revisions: false,
            visual_diff: false,
            objects_dump: false,
            extract_embedded: false,
            ..DomainOptions::default()
        };
        let a = run(&fixture("two-revisions.pdf"), dir.path(), &opts);
        assert!(a.outputs.is_empty(), "{:?}", a.outputs);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert_eq!(a.details["visual"]["status"], "not_applicable");
    }

    #[test]
    fn embedded_streams_are_written_raw_and_inflated() {
        use std::io::Write as _;
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(b"hello attachment").unwrap();
        let deflated = z.finish().unwrap();
        let mut file = b"%PDF-1.7\n".to_vec();
        let off_a = file.len() as u64;
        file.extend_from_slice(&deflated);
        let off_b = file.len() as u64;
        file.extend_from_slice(b"\xFF\xD8\xFF\xE0fakejpeg");
        let report = PdfReport {
            embedded: vec![
                rv_pdfauth_pdf::Embedded {
                    kind: "attachment".into(),
                    name: "notes/../x.txt".into(),
                    object: "9 0".into(),
                    revision: 1,
                    offset: off_a,
                    length: deflated.len() as u64,
                    filters: vec!["FlateDecode".into()],
                },
                rv_pdfauth_pdf::Embedded {
                    kind: "image".into(),
                    name: "Im1".into(),
                    object: "10 0".into(),
                    revision: 1,
                    offset: off_b,
                    length: 12,
                    filters: vec!["DCTDecode".into()],
                },
                rv_pdfauth_pdf::Embedded {
                    kind: "image".into(),
                    name: "out-of-range".into(),
                    offset: 10_000,
                    length: 5,
                    ..Default::default()
                },
            ],
            ..PdfReport::default()
        };
        let dir = tempfile::tempdir().unwrap();
        let mut w = Writer {
            dir: dir.path(),
            outputs: vec![],
            errors: vec![],
        };
        let v = write_embedded(&file, &report, &mut w);
        let rels: Vec<&str> = w.outputs.iter().map(|o| o.rel_path.as_str()).collect();
        assert_eq!(
            rels,
            [
                "embedded/01-notes_.._x.txt.bin",
                "embedded/01-notes_.._x.txt.inflated.bin",
                "embedded/02-Im1.jpg"
            ]
        );
        assert_eq!(w.outputs[2].kind, "embedded_image");
        assert_eq!(w.outputs[0].kind, "embedded_file");
        assert_eq!(
            std::fs::read(dir.path().join(rels[1])).unwrap(),
            b"hello attachment"
        );
        assert!(v[2]["status"].as_str().unwrap().contains("outside"));
        // Never overwrites: a second write of the same name fails and is reported.
        assert!(!w.write("embedded/02-Im1.jpg", "embedded_image", b"x"));
        assert_eq!(w.errors.len(), 1);
    }
}
