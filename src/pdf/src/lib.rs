//! PDF layer of RV PDFAuth. Pure functions over the raw bytes of one PDF: nothing here writes files,
//! prints, renders or touches the network. The core (`rv-pdfauth-core`) decides what to write and logs it.
//!
//! The parser reads hostile input, so this crate forbids `unsafe` and must never panic on any byte
//! sequence (fuzzed with cargo-fuzz). Every value it reports keeps its byte offset in the file.
#![forbid(unsafe_code)]

pub mod model;

pub use model::*;

/// Analyse one PDF. `bytes` is the whole file (memory-mapped by the caller, opened read-only).
/// FROZEN SIGNATURE: the core and the worker call exactly this.
pub fn analyze(bytes: &[u8], opts: &PdfOptions) -> PdfReport {
    let _ = opts;
    PdfReport {
        is_pdf: bytes.starts_with(b"%PDF-"),
        ..PdfReport::default()
    }
}

/// Bytes of revision `rev` (1-based) = the file up to and including that revision's `%%EOF` line.
/// FROZEN SIGNATURE.
pub fn revision_bytes<'a>(bytes: &'a [u8], report: &PdfReport, rev: usize) -> Option<&'a [u8]> {
    let r = report.revisions.get(rev.checked_sub(1)?)?;
    bytes.get(..usize::try_from(r.end_offset).ok()?)
}
