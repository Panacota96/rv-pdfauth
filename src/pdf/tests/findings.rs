//! One test per finding code (built in memory), plus structure checks.

mod common;
use common::{Builder, codes, sample, simple};
use rv_pdfauth_pdf::{PdfOptions, analyze, revision_bytes};

const INFO_RL: &str = "<< /Producer (ReportLab PDF Library - www.reportlab.com) /CreationDate (D:20260101100000Z) /ModDate (D:20260101100000Z) >>";

fn has(bytes: &[u8], code: &str) -> bool {
    codes(bytes).iter().any(|c| c == code)
}

#[test]
fn not_a_pdf() {
    assert_eq!(codes(b"hello world"), vec!["NOT_A_PDF"]);
    assert_eq!(codes(b""), vec!["NOT_A_PDF"]);
    assert!(!analyze(b"GIF89a", &PdfOptions::default()).is_pdf);
}

#[test]
fn clean_single_revision_has_no_findings() {
    let b = simple(
        "BT /F1 12 Tf 72 700 Td (Hello) Tj ET",
        "<< /Producer (pypdf) >>",
    )
    .bytes();
    let r = analyze(&b, &PdfOptions::default());
    assert!(r.is_pdf);
    assert_eq!(r.version.as_deref(), Some("1.7"));
    assert_eq!(r.revisions.len(), 1);
    assert_eq!(r.page_count, Some(1));
    assert!(r.revisions[0].xref_consistent);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
    assert_eq!(revision_bytes(&b, &r, 1).map(|s| s.len()), Some(b.len()));
    assert!(revision_bytes(&b, &r, 2).is_none());
    assert!(revision_bytes(&b, &r, 0).is_none());
}

#[test]
fn encrypted_partial_analysis() {
    let mut b = Builder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [] /Count 0 >>")
        .obj(3, "<< /Filter /Standard /V 2 /R 3 /O <00> /U <00> /P -4 >>")
        .revision("/Root 1 0 R /Encrypt 3 0 R");
    let r = analyze(&b.bytes(), &PdfOptions::default());
    assert!(r.encrypted);
    assert!(has(&b.bytes(), "ENCRYPTED_PARTIAL_ANALYSIS"));
}

fn two_revisions(info1: &str, info2: &str) -> Vec<u8> {
    let mut b = simple("BT /F1 12 Tf 72 700 Td (Hello) Tj ET", info1);
    b.obj(6, info2).revision("/Root 1 0 R /Info 6 0 R");
    b.bytes()
}

#[test]
fn incremental_updates_and_metadata_revision() {
    let b = two_revisions(
        INFO_RL,
        INFO_RL
            .replace("D:20260101100000Z) >>", "D:20260101100500Z) >>")
            .as_str(),
    );
    let r = analyze(&b, &PdfOptions::default());
    assert_eq!(r.revisions.len(), 2);
    assert_eq!(r.revisions[1].objects_changed, vec!["6 0"]);
    assert_eq!(r.revisions[1].change_kind, vec!["metadata"]);
    assert!(r.revisions[1].pages_changed.is_empty());
    assert!(has(&b, "INCREMENTAL_UPDATES"));
    assert!(
        !has(&b, "REVISION_TIME_GAP"),
        "5 minutes is below the threshold"
    );
}

#[test]
fn revision_time_gap() {
    let b = two_revisions(
        INFO_RL,
        INFO_RL
            .replace("D:20260101100000Z) >>", "D:20260111100000Z) >>")
            .as_str(),
    );
    let r = analyze(&b, &PdfOptions::default());
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "REVISION_TIME_GAP")
        .expect("gap");
    assert_eq!(f.params["gap"], "10 d 0 h 0 min");
    assert_eq!(f.params["from_rev"], "1");
}

#[test]
fn revision_date_not_monotonic() {
    // Revision 2 (appended later in the file) claims a save 3 days before revision 1.
    let b = two_revisions(
        "<< /Producer (pypdf) /CreationDate (D:20260101100000Z) /ModDate (D:20260110100000Z) >>",
        "<< /Producer (pypdf) /CreationDate (D:20260101100000Z) /ModDate (D:20260107100000Z) >>",
    );
    let r = analyze(&b, &PdfOptions::default());
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "REVISION_DATE_NOT_MONOTONIC")
        .expect("non-monotonic revision dates");
    assert_eq!(f.severity, rv_pdfauth_pdf::Severity::Medium);
    assert_eq!(f.params["from_rev"], "1");
    assert_eq!(f.params["to_rev"], "2");
    assert_eq!(f.params["from_date"], "2026-01-10T10:00:00Z");
    assert_eq!(f.params["to_date"], "2026-01-07T10:00:00Z");
    assert_eq!(f.params["gap"], "3 d 0 h 0 min");
    assert!(!has(&b, "REVISION_TIME_GAP"));
    // Equal or later dates give no finding.
    for later in ["D:20260110100000Z", "D:20260111100000Z"] {
        let ok = two_revisions(
            "<< /Producer (pypdf) /ModDate (D:20260110100000Z) >>",
            &format!("<< /Producer (pypdf) /ModDate ({}) >>", later),
        );
        assert!(!has(&ok, "REVISION_DATE_NOT_MONOTONIC"), "{}", later);
    }
}

#[test]
fn moddate_before_creationdate() {
    let b = simple(
        "",
        "<< /Producer (pypdf) /CreationDate (D:20260310120000+01'00') /ModDate (D:20260301120000+01'00') >>",
    )
    .bytes();
    assert!(has(&b, "MODDATE_BEFORE_CREATIONDATE"));
}

#[test]
fn producer_changed_between_revisions() {
    let b = two_revisions("<< /Producer (pypdf) >>", "<< /Producer (Nitro Pro 14) >>");
    let c = codes(&b);
    assert!(c.contains(&"PRODUCER_CHANGED_BETWEEN_REVISIONS".to_string()));
    assert!(c.contains(&"EDITOR_FINGERPRINT".to_string()));
}

#[test]
fn online_editor_or_unlocker() {
    let b = simple("", "<< /Producer (Smallpdf.com) >>").bytes();
    assert!(has(&b, "ONLINE_EDITOR_OR_UNLOCKER"));
    let b = simple("", "<< /Producer (PDF Password Remover v3) >>").bytes();
    assert!(has(&b, "ONLINE_EDITOR_OR_UNLOCKER"));
}

#[test]
fn producer_structure_mismatch() {
    // ReportLab writes its own header comment; this file claims ReportLab without it.
    let b = simple("", INFO_RL).bytes();
    let r = analyze(&b, &PdfOptions::default());
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "PRODUCER_STRUCTURE_MISMATCH")
        .expect("mismatch");
    assert!(f.params["traits"].contains("header_comment_contains"));
}

#[test]
fn unknown_producer() {
    let b = simple("", "<< /Producer (Acme Writer 1.0) >>").bytes();
    assert_eq!(codes(&b), vec!["UNKNOWN_PRODUCER"]);
}

#[test]
fn metadata_info_xmp_mismatch() {
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:pdf="http://ns.adobe.com/pdf/1.3/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" pdf:Producer="pypdf"><xmp:CreateDate>2026-01-01T10:00:00Z</xmp:CreateDate></rdf:Description></rdf:RDF></x:xmpmeta>"#;
    let mut b = Builder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [] /Count 0 >>")
        .stream(7, "/Type /Metadata /Subtype /XML", xmp.as_bytes())
        .obj(
            6,
            "<< /Producer (pdf-lib) /CreationDate (D:20260105100000Z) >>",
        )
        .revision("/Root 1 0 R /Info 6 0 R");
    let r = analyze(&b.bytes(), &PdfOptions::default());
    let x = r.revisions[0].xmp.as_ref().expect("xmp");
    assert_eq!(x.producer.as_deref(), Some("pypdf"));
    let fields: Vec<&str> = r
        .findings
        .iter()
        .filter(|f| f.code == "METADATA_INFO_XMP_MISMATCH")
        .map(|f| f.params["field"].as_str())
        .collect();
    assert_eq!(fields, vec!["Producer", "CreationDate"]);
}

#[test]
fn xref_offsets_inconsistent() {
    let mut b = simple("", "<< /Producer (pypdf) >>").bytes();
    // Corrupt the offset of object 2 in the xref table.
    let t = String::from_utf8_lossy(&b).into_owned();
    let x = t.rfind("2 1\n").unwrap() + 4;
    b[x + 9] = if b[x + 9] == b'9' { b'0' } else { b[x + 9] + 1 };
    let r = analyze(&b, &PdfOptions::default());
    assert!(!r.revisions[0].xref_consistent);
    assert!(has(&b, "XREF_OFFSETS_INCONSISTENT"));
    assert!(!r.warnings.is_empty());
    // The object is still resolved (from the scan, with a warning), so the page is found.
    assert_eq!(r.page_count, Some(1));
}

#[test]
fn data_after_final_eof() {
    let mut b = simple("", "<< /Producer (pypdf) >>").bytes();
    assert!(!has(&b, "DATA_AFTER_FINAL_EOF"));
    b.extend_from_slice(b"\r\n\n");
    assert!(!has(&b, "DATA_AFTER_FINAL_EOF"), "whitespace only");
    b.extend_from_slice(b"PK\x03\x04 hidden zip");
    let r = analyze(&b, &PdfOptions::default());
    assert_eq!(r.trailing_bytes, 18);
    assert!(has(&b, "DATA_AFTER_FINAL_EOF"));
}

#[test]
fn hybrid_xref() {
    // Revision with a classic table whose trailer points to an xref stream (/XRefStm).
    let mut b = Builder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [] /Count 0 >>");
    let off2 = b.buf.len();
    b.obj(3, "<< /Producer (pypdf) >>");
    let xs = b.buf.len();
    let mut rows = Vec::new();
    rows.extend_from_slice(&[1, (off2 >> 8) as u8, off2 as u8, 0]);
    let rows_len = rows.len();
    b.raw(
        format!(
            "9 0 obj\n<< /Type /XRef /W [1 2 1] /Index [3 1] /Size 10 /Length {} >>\nstream\n",
            rows_len
        )
        .as_bytes(),
    );
    b.raw(&rows).raw(b"\nendstream\nendobj\n");
    b.revision(&format!("/Root 1 0 R /Info 3 0 R /XRefStm {}", xs));
    let bytes = b.bytes();
    let r = analyze(&bytes, &PdfOptions::default());
    assert_eq!(r.revisions[0].xref_kind, "hybrid");
    assert!(has(&bytes, "HYBRID_XREF"));
}

#[test]
fn javascript_and_launch() {
    let mut b = Builder::new();
    b.obj(
        1,
        "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert\\(1\\)) >> >>",
    )
    .obj(2, "<< /Type /Pages /Kids [] /Count 0 >>")
    .obj(3, "<< /S /Launch /F (cmd.exe) >>")
    .revision("/Root 1 0 R");
    let c = codes(&b.bytes());
    assert!(c.contains(&"JAVASCRIPT_PRESENT".to_string()));
    assert!(c.contains(&"LAUNCH_ACTION".to_string()));
}

#[test]
fn embedded_files() {
    let mut b = Builder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [(a.txt) 3 0 R] >> >> >>")
        .obj(2, "<< /Type /Pages /Kids [] /Count 0 >>")
        .obj(3, "<< /Type /Filespec /F (a.txt) /UF (a.txt) /EF << /F 4 0 R >> >>")
        .stream(4, "/Type /EmbeddedFile", b"secret")
        .revision("/Root 1 0 R");
    let bytes = b.bytes();
    let r = analyze(
        &bytes,
        &PdfOptions {
            embedded: true,
            ..Default::default()
        },
    );
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "EMBEDDED_FILES")
        .expect("embedded");
    assert_eq!(f.params["count"], "1");
    assert_eq!(r.embedded.len(), 1);
    let e = &r.embedded[0];
    assert_eq!(
        (e.kind.as_str(), e.name.as_str(), e.object.as_str()),
        ("attachment", "a.txt", "4 0")
    );
    assert_eq!(
        &bytes[e.offset as usize..(e.offset + e.length) as usize],
        b"secret"
    );
    // Without the option the list stays empty but the finding remains.
    let r = analyze(&bytes, &PdfOptions::default());
    assert!(r.embedded.is_empty());
}

#[test]
fn invisible_text_render_mode_and_white_fill() {
    let b = simple(
        "BT /F1 12 Tf 3 Tr 72 700 Td (hidden) Tj ET",
        "<< /Producer (pypdf) >>",
    )
    .bytes();
    assert!(has(&b, "INVISIBLE_TEXT"));
    let b = simple(
        "1 g BT /F1 12 Tf 72 700 Td (white) Tj ET",
        "<< /Producer (pypdf) >>",
    )
    .bytes();
    assert!(has(&b, "INVISIBLE_TEXT"));
    let b = simple(
        "q 1 g Q BT /F1 12 Tf 72 700 Td (black) Tj ET",
        "<< /Producer (pypdf) >>",
    )
    .bytes();
    assert!(!has(&b, "INVISIBLE_TEXT"), "q/Q restores the fill colour");
    let b = simple(
        "BT /F1 12 Tf 3 Tr 72 700 Td (   ) Tj ET",
        "<< /Producer (pypdf) >>",
    )
    .bytes();
    assert!(!has(&b, "INVISIBLE_TEXT"), "spaces only");
}

fn scan_page(content: &str) -> Builder {
    let mut b = Builder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im0 5 0 R >> /Font << /F1 6 0 R >> >> /Contents 4 0 R >>",
        )
        .stream(4, "", content.as_bytes())
        .stream(5, "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"\x80")
        .obj(6, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        .obj(7, "<< /Producer (pypdf) >>")
        .revision("/Root 1 0 R /Info 7 0 R");
    b
}

#[test]
fn scanned_pages_and_text_over_image() {
    let b = scan_page("q 612 0 0 792 0 0 cm /Im0 Do Q").bytes();
    assert_eq!(codes(&b), vec!["SCANNED_PAGES_STRUCTURE_ONLY"]);
    let b = scan_page("q 100 0 0 100 0 0 cm /Im0 Do Q").bytes();
    assert!(codes(&b).is_empty(), "small image is not a scan");
    // Text layer added in a later revision.
    let mut b = scan_page("q 612 0 0 792 0 0 cm /Im0 Do Q");
    b.stream(8, "", b"BT /F1 12 Tf 72 700 Td (USD 9,999) Tj ET")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im0 5 0 R >> /Font << /F1 6 0 R >> >> /Contents [4 0 R 8 0 R] >>",
        )
        .revision("/Root 1 0 R /Info 7 0 R");
    let r = analyze(&b.bytes(), &PdfOptions::default());
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "TEXT_OVER_SCANNED_IMAGE")
        .expect("overlay");
    assert_eq!(f.params["page"], "1");
    assert_eq!(f.params["text_revision"], "2");
    assert_eq!(r.revisions[1].pages_changed, vec![1]);
    assert_eq!(r.revisions[1].change_kind, vec!["other"]);
}

#[test]
fn font_added_in_later_revision() {
    let mut b = simple(
        "BT /F1 12 Tf 72 700 Td (a) Tj ET",
        "<< /Producer (pypdf) >>",
    );
    b.obj(7, "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+Arial-BoldMT /FontDescriptor 8 0 R >>")
        .obj(8, "<< /Type /FontDescriptor /FontName /ABCDEF+Arial-BoldMT /FontFile2 9 0 R >>")
        .stream(9, "", b"\x00\x01\x00\x00")
        .stream(10, "", b"BT /F2 12 Tf 72 680 Td (b) Tj ET")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R /F2 7 0 R >> >> /Contents [4 0 R 10 0 R] >>",
        )
        .revision("/Root 1 0 R /Info 6 0 R");
    let r = analyze(&b.bytes(), &PdfOptions::default());
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "FONT_ADDED_IN_LATER_REVISION")
        .expect("font");
    assert_eq!(f.params["font"], "ABCDEF+Arial-BoldMT");
    assert_eq!(f.params["revision"], "2");
    let font = r
        .fonts
        .iter()
        .find(|f| f.name == "ABCDEF+Arial-BoldMT")
        .unwrap();
    assert_eq!(font.subset_prefix.as_deref(), Some("ABCDEF"));
    assert!(font.embedded);
    assert_eq!(font.first_revision, 2);
}

#[test]
fn signature_checks_on_the_signed_sample() {
    let full = sample("signed-then-modified.pdf");
    let r = analyze(&full, &PdfOptions::default());
    assert_eq!(r.signatures.len(), 1);
    let s = &r.signatures[0];
    assert_eq!(s.sub_filter.as_deref(), Some("adbe.pkcs7.detached"));
    assert_eq!(s.revision, Some(2));
    assert!(s.covers_revision);
    assert_eq!((s.digest_ok, s.signature_ok), (Some(true), Some(true)));
    assert_eq!(s.docmdp, Some(2));
    assert_eq!(s.later_revisions, vec![3]);
    assert!(
        s.signer_subject
            .as_deref()
            .unwrap_or("")
            .contains("Ana Ruiz")
    );
    assert_eq!(r.revisions[1].signed_by, vec!["Signature1"]);
    assert_eq!(r.revisions[1].change_kind, vec!["signature"]);
    let c = codes(&full);
    for code in [
        "CONTENT_CHANGED_AFTER_SIGNATURE",
        "DOCMDP_VIOLATION",
        "SIGNATURE_PRESENT_TRUST_NOT_VERIFIED",
    ] {
        assert!(c.contains(&code.to_string()), "{}", code);
    }
    assert!(!c.contains(&"SIGNATURE_INTEGRITY_FAILED".to_string()));

    // The signed revision alone: no change after signing.
    let rev2 = revision_bytes(&full, &r, 2).unwrap().to_vec();
    let c2 = codes(&rev2);
    assert!(!c2.contains(&"CONTENT_CHANGED_AFTER_SIGNATURE".to_string()));
    assert!(!c2.contains(&"DOCMDP_VIOLATION".to_string()));

    // One byte changed inside the signed range: the message digest no longer matches.
    let mut bad = rev2.clone();
    let pos = String::from_utf8_lossy(&bad)
        .find("SAMPLE - signed")
        .unwrap();
    bad[pos] = b'a';
    let r = analyze(&bad, &PdfOptions::default());
    assert_eq!(r.signatures[0].digest_ok, Some(false));
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "SIGNATURE_INTEGRITY_FAILED")
        .expect("integrity");
    assert!(f.params["reason"].contains("digest_mismatch"));

    // A byte changed inside the CMS signature value: signature maths fail.
    let mut bad = rev2.clone();
    let br = &r.signatures[0].byte_range;
    let sig_hex_end = br[2] as usize - 2;
    let p = (br[1] as usize..sig_hex_end)
        .rev()
        .find(|&i| bad[i] != b'0')
        .unwrap()
        - 40;
    bad[p] = if bad[p] == b'a' { b'b' } else { b'a' };
    let r = analyze(&bad, &PdfOptions::default());
    assert_eq!(r.signatures[0].digest_ok, Some(true));
    assert_eq!(r.signatures[0].signature_ok, Some(false));
    let f = r
        .findings
        .iter()
        .find(|f| f.code == "SIGNATURE_INTEGRITY_FAILED")
        .expect("integrity");
    assert_eq!(f.params["reason"], "signature_invalid");
}

#[test]
fn object_tree_and_xref_stream_sample() {
    let b = sample("online-editor.pdf");
    let r = analyze(
        &b,
        &PdfOptions {
            object_tree: true,
            ..Default::default()
        },
    );
    assert_eq!(r.revisions[0].xref_kind, "stream");
    let objs = r.object_tree["revisions"][0]["objects"].as_array().unwrap();
    assert!(objs.iter().any(|o| o["type"] == "Catalog"));
    assert!(
        objs.iter()
            .any(|o| o["in_object_stream"].as_u64().is_some())
    );
}

#[test]
fn starter_db_is_exposed() {
    let db = rv_pdfauth_pdf::starter_db();
    assert!(db.entries.len() >= 25);
    // Extra databases from the core are used too.
    let extra = rv_pdfauth_pdf::FingerprintDb {
        name: "case-learned".into(),
        version: "1".into(),
        entries: vec![rv_pdfauth_pdf::FingerprintEntry {
            id: "acme".into(),
            label: "Acme Writer".into(),
            category: "pdf_editor".into(),
            producer_contains: vec!["acme writer".into()],
            source: "learned from case TEST".into(),
            ..Default::default()
        }],
    };
    let b = simple("", "<< /Producer (Acme Writer 1.0) >>").bytes();
    let r = analyze(
        &b,
        &PdfOptions {
            fingerprints: vec![extra],
            ..Default::default()
        },
    );
    assert!(r.findings.iter().any(|f| f.code == "EDITOR_FINGERPRINT"));
    assert!(!r.findings.iter().any(|f| f.code == "UNKNOWN_PRODUCER"));
}
