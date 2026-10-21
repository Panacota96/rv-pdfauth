//! FROZEN data model shared by the PDF layer, the renderer/worker and the reports.
//! Add fields freely (serde default); never rename or remove one without bumping the report schema.
//! Every number that is a position in the file is a byte offset (u64). Dates are kept as found
//! (`raw`) plus an ISO-8601 UTC form when they parse (`utc`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PdfOptions {
    /// Extra fingerprint databases (JSON, same format as fingerprints/starter.json), already loaded by the core.
    pub fingerprints: Vec<FingerprintDb>,
    /// Include the full object tree in `PdfReport::object_tree` (can be large).
    pub object_tree: bool,
    /// Collect embedded files and images in `PdfReport::embedded`.
    pub embedded: bool,
}

#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Info,
    Low,
    Medium,
    High,
}

/// Same shape as `rv_pdfauth_core::analyzer::Finding` (code worded in i18n, string params only).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PdfFinding {
    pub code: String,
    pub severity: Severity,
    pub params: BTreeMap<String, String>,
}

impl PdfFinding {
    pub fn new(code: &str, severity: Severity) -> Self {
        PdfFinding {
            code: code.into(),
            severity,
            params: BTreeMap::new(),
        }
    }
    pub fn with(mut self, k: &str, v: impl Into<String>) -> Self {
        self.params.insert(k.into(), v.into());
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PdfReport {
    pub is_pdf: bool,
    /// Header version, e.g. "1.7".
    pub version: Option<String>,
    pub encrypted: bool,
    pub page_count: Option<u32>,
    /// Oldest first. Revision 1 = original save; each incremental update adds one.
    pub revisions: Vec<Revision>,
    pub signatures: Vec<Signature>,
    pub fonts: Vec<FontUse>,
    pub embedded: Vec<Embedded>,
    /// Producer/editor matches across all revisions.
    pub fingerprints: Vec<FingerprintMatch>,
    /// Bytes after the last `%%EOF` (0 = none).
    pub trailing_bytes: u64,
    /// Parser warnings (never silently repaired): offset + message.
    pub warnings: Vec<ParseWarning>,
    pub findings: Vec<PdfFinding>,
    /// Offsets of every `%PDF-` header found outside objects (more than one: concatenated files).
    pub headers: Vec<u64>,
    /// Full object tree by revision (only when PdfOptions::object_tree): {"revisions":[{"objects":[{"id":"12 0","offset":..,"type":..,"dict":{..}}]}]}
    pub object_tree: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Revision {
    /// 1-based.
    pub index: usize,
    /// First byte of this revision's body (0 for revision 1).
    pub start_offset: u64,
    /// One past the end of this revision's `%%EOF` line: revision_bytes = file[..end_offset].
    pub end_offset: u64,
    pub startxref: Option<u64>,
    /// "table", "stream" or "hybrid".
    pub xref_kind: String,
    /// Do the xref entries point at the objects they claim? false + warning when not.
    pub xref_consistent: bool,
    pub objects_added: Vec<String>,
    pub objects_changed: Vec<String>,
    pub objects_deleted: Vec<String>,
    /// Pages whose content streams/resources changed in this revision (1-based), empty for revision 1.
    pub pages_changed: Vec<u32>,
    pub info: InfoDict,
    pub xmp: Option<Xmp>,
    /// Trailer /ID pair as hex.
    pub id: Vec<String>,
    /// Signature field names signed by this revision (its ByteRange ends at end_offset).
    pub signed_by: Vec<String>,
    /// What this revision changes, classified for DocMDP: "form_fill", "annotations", "signature", "other",
    /// plus "metadata" (only the Info dictionary / XMP stream changed).
    pub change_kind: Vec<String>,
    /// Number of pages in this revision's page tree (None when it has no catalog).
    pub page_count: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PdfDate {
    pub raw: String,
    pub utc: Option<String>,
    /// The date had no time-zone offset; `utc` then assumes UTC.
    pub tz_missing: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct InfoDict {
    pub producer: Option<String>,
    pub creator: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub creation_date: Option<PdfDate>,
    pub mod_date: Option<PdfDate>,
    /// Object id of the Info dictionary, e.g. "7 0".
    pub object: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Xmp {
    pub producer: Option<String>,
    pub creator_tool: Option<String>,
    pub create_date: Option<String>,
    pub modify_date: Option<String>,
    pub metadata_date: Option<String>,
    pub document_id: Option<String>,
    pub instance_id: Option<String>,
    /// xmpMM:History entries: action, when, softwareAgent.
    pub history: Vec<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Signature {
    pub field: String,
    pub sub_filter: Option<String>,
    pub byte_range: Vec<u64>,
    /// Revision whose end the ByteRange reaches.
    pub revision: Option<usize>,
    /// ByteRange covers the whole revision except /Contents.
    pub covers_revision: bool,
    /// None = not checked (unsupported algorithm); Some(false) = mismatch.
    pub digest_ok: Option<bool>,
    pub signature_ok: Option<bool>,
    pub signer_subject: Option<String>,
    pub signing_time: Option<String>,
    /// DocMDP permission level 1–3 if certified.
    pub docmdp: Option<u8>,
    /// Revisions added after this signature and how they are classified.
    pub later_revisions: Vec<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FontUse {
    pub name: String,
    /// "ABCDEF" for subset fonts.
    pub subset_prefix: Option<String>,
    pub embedded: bool,
    pub first_revision: usize,
    pub object: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Embedded {
    /// "attachment" or "image".
    pub kind: String,
    pub name: String,
    pub object: String,
    pub revision: usize,
    /// Byte range of the (still encoded) stream data in the file; the core copies/decodes it.
    pub offset: u64,
    pub length: u64,
    /// /Filter names, e.g. ["DCTDecode"]. DCTDecode streams are written as .jpg, others as .bin.
    pub filters: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ParseWarning {
    pub offset: u64,
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FingerprintDb {
    pub name: String,
    pub version: String,
    pub entries: Vec<FingerprintEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FingerprintEntry {
    pub id: String,
    pub label: String,
    /// office, pdf_library, scanner, pdf_editor, online_editor, unlocker, printer_driver, browser
    pub category: String,
    /// Case-insensitive regexes-free substrings matched against Producer / Creator / XMP CreatorTool.
    pub producer_contains: Vec<String>,
    pub creator_contains: Vec<String>,
    /// Structural traits the producer is known to have: e.g. {"xref_kind":"stream","object_streams":"yes"}.
    pub traits: BTreeMap<String, String>,
    /// "vendor documentation", "generated by Rakvor on <date>", "learned from case <id>".
    pub source: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FingerprintMatch {
    pub revision: usize,
    pub db: String,
    pub entry_id: String,
    pub label: String,
    pub category: String,
    /// "producer", "creator", "xmp_creator_tool", "structure".
    pub matched_on: String,
    /// Traits of the entry that the file does NOT have (→ PRODUCER_STRUCTURE_MISMATCH).
    pub trait_mismatches: Vec<String>,
    /// The metadata string (or header comment) that matched.
    pub matched_value: String,
}
