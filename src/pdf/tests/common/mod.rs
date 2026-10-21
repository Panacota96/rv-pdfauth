//! Tiny PDF writer for tests: objects, streams, incremental revisions with correct xref tables.
#![allow(dead_code)]

use std::collections::BTreeMap;

pub struct Builder {
    pub buf: Vec<u8>,
    pending: BTreeMap<u32, usize>,
    prev: Option<usize>,
    size: u32,
}

impl Builder {
    pub fn new() -> Self {
        Builder {
            buf: b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec(),
            pending: BTreeMap::new(),
            prev: None,
            size: 1,
        }
    }

    pub fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    pub fn obj(&mut self, num: u32, body: &str) -> &mut Self {
        self.pending.insert(num, self.buf.len());
        self.buf
            .extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", num, body).as_bytes());
        self.size = self.size.max(num + 1);
        self
    }

    pub fn stream(&mut self, num: u32, dict_extra: &str, data: &[u8]) -> &mut Self {
        self.pending.insert(num, self.buf.len());
        self.buf.extend_from_slice(
            format!(
                "{} 0 obj\n<< /Length {} {} >>\nstream\n",
                num,
                data.len(),
                dict_extra
            )
            .as_bytes(),
        );
        self.buf.extend_from_slice(data);
        self.buf.extend_from_slice(b"\nendstream\nendobj\n");
        self.size = self.size.max(num + 1);
        self
    }

    /// Ends a revision: xref table for the objects written since the last revision, trailer, %%EOF.
    pub fn revision(&mut self, trailer_extra: &str) -> &mut Self {
        let x = self.buf.len();
        let mut t = String::from("xref\n");
        let first = self.prev.is_none();
        if first {
            t.push_str("0 1\n0000000000 65535 f\r\n");
        }
        for (num, off) in &self.pending {
            t.push_str(&format!("{} 1\n{:010} 00000 n\r\n", num, off));
        }
        let prev = self
            .prev
            .map(|p| format!(" /Prev {}", p))
            .unwrap_or_default();
        t.push_str(&format!(
            "trailer\n<< /Size {}{} {} >>\nstartxref\n{}\n%%EOF\n",
            self.size, prev, trailer_extra, x
        ));
        self.buf.extend_from_slice(t.as_bytes());
        self.pending.clear();
        self.prev = Some(x);
        self
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.buf.clone()
    }
}

/// One-page document: catalog 1, pages 2, page 3 with content 4 and font 5; Info 6.
pub fn simple(content: &str, info: &str) -> Builder {
    let mut b = Builder::new();
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        )
        .stream(4, "", content.as_bytes())
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        .obj(6, info)
        .revision("/Root 1 0 R /Info 6 0 R");
    b
}

pub fn codes(bytes: &[u8]) -> Vec<String> {
    let r = rv_pdfauth_pdf::analyze(bytes, &rv_pdfauth_pdf::PdfOptions::default());
    let mut c: Vec<String> = r.findings.iter().map(|f| f.code.clone()).collect();
    c.sort();
    c.dedup();
    c
}

pub fn sample(name: &str) -> Vec<u8> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/sample/evidence")
        .join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {}", p.display(), e))
}
