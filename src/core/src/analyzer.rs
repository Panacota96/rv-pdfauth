//! Domain logic — THE PART EACH NEW TOOL REPLACES.
//!
//! This reference analyzer inventories evidence: it detects the real file type from its
//! signature (magic bytes), flags extension mismatches, executables under other names and
//! empty files. Duplicates are found later, across items, by `run.rs`.
//! A new tool keeps the same `analyze` signature and `Finding` shape so the rest of the
//! pipeline (audit, reports, CLI, GUI) works unchanged.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
}

impl Severity {
    pub fn key(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
        }
    }
}

/// One finding: a stable code (worded in i18n/*.json), a severity and named parameters.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    pub code: String,
    pub severity: Severity,
    pub params: BTreeMap<String, String>,
}

impl Finding {
    pub fn new(code: &str, severity: Severity) -> Finding {
        Finding {
            code: code.into(),
            severity,
            params: BTreeMap::new(),
        }
    }
    pub fn with(mut self, k: &str, v: impl Into<String>) -> Finding {
        self.params.insert(k.into(), v.into());
        self
    }
}

/// Also the payload a worker process prints on stdout (see `worker.rs`), hence Deserialize.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Analysis {
    pub detected_type: Option<String>,
    pub extension: Option<String>,
    pub findings: Vec<Finding>,
    /// Domain data for the technical report (rv-pdfauth: the `PdfReport` + visual-diff summary). Null if none.
    pub details: serde_json::Value,
    /// Files the analysis wrote under the item folder; the pipeline hashes and audits every one.
    pub outputs: Vec<OutputFile>,
}

/// One file written by the analysis, relative to the item report folder (forward slashes).
#[derive(Clone, Debug, Default, Serialize, serde::Deserialize, PartialEq)]
pub struct OutputFile {
    pub rel_path: String,
    /// revision, visual_diff, objects_dump, embedded_file, embedded_image, page_render
    pub kind: String,
}

/// How heavy analysis runs. Subprocess = `<exe> worker …` per item, so a crash in native code (PDFium) or a
/// hang cannot stop a bulk run or corrupt the audit log; InProcess is for unit tests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Isolation {
    #[default]
    InProcess,
    Subprocess {
        exe: std::path::PathBuf,
    },
}

/// Domain options (FROZEN names; the CLI flags and GUI settings map 1:1 onto them).
/// Serialisable: the parent passes them to a worker process as JSON (`--opts`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DomainOptions {
    pub extract_revisions: bool,
    pub visual_diff: bool,
    pub dpi: u32,
    pub max_pages: u32,
    pub objects_dump: bool,
    pub extract_embedded: bool,
    pub worker_timeout_s: u64,
    pub worker_memory_mb: u64,
    /// Extra fingerprint database (JSON) on top of the bundled starter DB and the local learned DB.
    pub fingerprints_db: Option<std::path::PathBuf>,
    /// Write item reports only for items with findings above info.
    pub item_reports_flagged_only: bool,
    pub isolation: Isolation,
}

impl Default for DomainOptions {
    fn default() -> Self {
        DomainOptions {
            extract_revisions: true,
            visual_diff: true,
            dpi: 110,
            max_pages: 50,
            objects_dump: true,
            extract_embedded: true,
            worker_timeout_s: 120,
            worker_memory_mb: 2048,
            fingerprints_db: None,
            item_reports_flagged_only: false,
            isolation: Isolation::InProcess,
        }
    }
}

/// Everything the domain analysis gets for one item. `item_dir` exists and is empty; write outputs only there.
pub struct AnalyzeCtx<'a> {
    pub path: &'a std::path::Path,
    pub file_name: &'a str,
    pub size: u64,
    /// First 4 KiB, already read during hashing.
    pub head: &'a [u8],
    pub item_dir: &'a std::path::Path,
    pub opts: &'a DomainOptions,
    /// Digest of the first (hashing) pass with the run's primary algorithm. The PDF analysis re-hashes the
    /// bytes it actually parses (the memory map, in the worker) and compares: a difference is the finding
    /// `EVIDENCE_CHANGED_DURING_ANALYSIS`. None = no comparison (unit tests, items that are not PDFs).
    pub expected_digest: Option<(crate::hash::HashAlgo, &'a str)>,
}

/// FROZEN entry point called by run.rs for every readable item. PDF candidates (`%PDF-` header or `.pdf`
/// extension) get the PDF analysis — in a worker process with [`Isolation::Subprocess`], so a crash, hang or
/// memory blow-up in the parser or in PDFium becomes a `PARSER_FAILED` finding instead of ending the run.
/// Everything else keeps the reference inventory logic (`analyze`), in process.
pub fn analyze_item(ctx: &AnalyzeCtx) -> Analysis {
    if !crate::pdfscan::is_pdf_candidate(ctx.file_name, ctx.head) {
        return analyze(ctx.file_name, ctx.size, ctx.head);
    }
    match &ctx.opts.isolation {
        Isolation::InProcess => crate::pdfscan::analyze_pdf(ctx),
        Isolation::Subprocess { exe } => crate::worker::analyze_in_worker(exe, ctx),
    }
}

/// The part of `Analysis::details` that `run.rs` writes into the `item_analyzed` audit entry: which
/// fingerprint databases (name, version, SHA-256) and which PDFium build (path, SHA-256) were used, how the
/// worker ended and whether the visual diff ran. Small and float-free, so the audit chain stays canonical.
pub fn audit_summary(details: &serde_json::Value) -> serde_json::Value {
    if !details.is_object() {
        return serde_json::Value::Null;
    }
    let mut m = serde_json::Map::new();
    for k in [
        "fingerprint_dbs",
        "worker",
        "revision_source",
        "evidence_rehash",
    ] {
        if let Some(v) = details.get(k) {
            m.insert(k.into(), v.clone());
        }
    }
    if let Some(v) = details.get("visual") {
        let mut vis = serde_json::Map::new();
        for k in [
            "status",
            "reason",
            "renderer",
            "pairs_compared",
            "pages_changed",
        ] {
            if let Some(x) = v.get(k) {
                vis.insert(k.into(), x.clone());
            }
        }
        m.insert("visual".into(), vis.into());
    }
    if m.is_empty() {
        serde_json::Value::Null
    } else {
        m.into()
    }
}

struct Signature {
    name: &'static str,
    offset: usize,
    magic: &'static [u8],
    extensions: &'static [&'static str],
    executable: bool,
}

const SIGNATURES: &[Signature] = &[
    Signature {
        name: "PDF document",
        offset: 0,
        magic: b"%PDF-",
        extensions: &["pdf"],
        executable: false,
    },
    Signature {
        name: "PNG image",
        offset: 0,
        magic: b"\x89PNG\r\n\x1a\n",
        extensions: &["png"],
        executable: false,
    },
    Signature {
        name: "JPEG image",
        offset: 0,
        magic: b"\xFF\xD8\xFF",
        extensions: &["jpg", "jpeg", "jpe", "jfif"],
        executable: false,
    },
    Signature {
        name: "GIF image",
        offset: 0,
        magic: b"GIF8",
        extensions: &["gif"],
        executable: false,
    },
    Signature {
        name: "ZIP / Office Open XML",
        offset: 0,
        magic: b"PK\x03\x04",
        extensions: &[
            "zip", "docx", "xlsx", "pptx", "odt", "ods", "jar", "apk", "epub",
        ],
        executable: false,
    },
    Signature {
        name: "OLE2 (legacy Office, MSG)",
        offset: 0,
        magic: b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
        extensions: &["doc", "xls", "ppt", "msg"],
        executable: false,
    },
    Signature {
        name: "Windows executable (PE)",
        offset: 0,
        magic: b"MZ",
        extensions: &["exe", "dll", "sys", "scr", "com", "efi"],
        executable: true,
    },
    Signature {
        name: "Linux executable (ELF)",
        offset: 0,
        magic: b"\x7fELF",
        extensions: &["so", "bin", "elf", "o"],
        executable: true,
    },
    Signature {
        name: "SQLite database",
        offset: 0,
        magic: b"SQLite format 3\0",
        extensions: &["db", "sqlite", "sqlite3"],
        executable: false,
    },
    Signature {
        name: "7-Zip archive",
        offset: 0,
        magic: b"7z\xBC\xAF\x27\x1C",
        extensions: &["7z"],
        executable: false,
    },
    Signature {
        name: "RAR archive",
        offset: 0,
        magic: b"Rar!\x1A\x07",
        extensions: &["rar"],
        executable: false,
    },
    Signature {
        name: "GZIP archive",
        offset: 0,
        magic: b"\x1F\x8B",
        extensions: &["gz", "tgz"],
        executable: false,
    },
    Signature {
        name: "Windows event log (EVTX)",
        offset: 0,
        magic: b"ElfFile\0",
        extensions: &["evtx"],
        executable: false,
    },
    Signature {
        name: "Windows registry hive",
        offset: 0,
        magic: b"regf",
        extensions: &["dat", "hve"],
        executable: false,
    },
    Signature {
        name: "MP4 / QuickTime video",
        offset: 4,
        magic: b"ftyp",
        extensions: &["mp4", "m4v", "mov", "m4a", "3gp", "heic"],
        executable: false,
    },
];

fn detect(head: &[u8]) -> Option<&'static Signature> {
    SIGNATURES.iter().find(|s| {
        head.len() >= s.offset + s.magic.len()
            && &head[s.offset..s.offset + s.magic.len()] == s.magic
    })
}

fn known_extension(ext: &str) -> bool {
    SIGNATURES.iter().any(|s| s.extensions.contains(&ext))
}

/// Analyzes one item from its name, size and first bytes (no second read of the evidence).
pub fn analyze(file_name: &str, size: u64, head: &[u8]) -> Analysis {
    let extension = std::path::Path::new(file_name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let sig = detect(head);
    let mut findings = Vec::new();
    if size == 0 {
        findings.push(Finding::new("EMPTY_FILE", Severity::Info));
    }
    if let (Some(s), Some(ext)) = (sig, extension.as_deref()) {
        if !s.extensions.contains(&ext) {
            if s.executable {
                findings.push(
                    Finding::new("EXECUTABLE_CONTENT", Severity::High)
                        .with("detected", s.name)
                        .with("extension", ext),
                );
            } else if known_extension(ext)
                || matches!(ext, "txt" | "csv" | "log" | "xml" | "json" | "html")
            {
                findings.push(
                    Finding::new("EXTENSION_MISMATCH", Severity::Medium)
                        .with("detected", s.name)
                        .with("extension", ext),
                );
            }
        }
    }
    Analysis {
        detected_type: sig.map(|s| s.name.to_string()),
        extension,
        findings,
        ..Analysis::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_types_and_mismatches() {
        let a = analyze("report.pdf", 10, b"%PDF-1.7\n");
        assert_eq!(a.detected_type.as_deref(), Some("PDF document"));
        assert!(a.findings.is_empty());

        let b = analyze("holiday.jpg", 10, b"%PDF-1.7\n");
        assert_eq!(b.findings[0].code, "EXTENSION_MISMATCH");

        let c = analyze("invoice.pdf", 10, b"MZ\x90\x00");
        assert_eq!(c.findings[0].code, "EXECUTABLE_CONTENT");
        assert_eq!(c.findings[0].severity, Severity::High);

        let d = analyze("empty.txt", 0, b"");
        assert_eq!(d.findings[0].code, "EMPTY_FILE");

        let e = analyze("notes.md", 10, b"hello");
        assert!(e.detected_type.is_none() && e.findings.is_empty());

        let f = analyze("clip.mov", 12, b"\0\0\0\x18ftypqt  ");
        assert!(f.findings.is_empty());
    }
}
