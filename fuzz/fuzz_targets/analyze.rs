#![no_main]
//! `analyze()` must never panic, overflow the stack or hang on any input.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let opts = rv_pdfauth_pdf::PdfOptions {
        object_tree: true,
        embedded: true,
        ..Default::default()
    };
    let report = rv_pdfauth_pdf::analyze(data, &opts);
    for r in &report.revisions {
        let _ = rv_pdfauth_pdf::revision_bytes(data, &report, r.index);
    }
});
