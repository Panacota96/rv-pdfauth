//! Visual diff between consecutive revisions: render every page present in both (up to `max_pages`) with
//! PDFium, compare the pixels, write `visual/rev-NN-vs-MM-page-PPP.png` for each changed page and raise
//! `VISUAL_CHANGE_BETWEEN_REVISIONS`. Without PDFium the run goes on: status `skipped`, the reason in the
//! details and an informational `VISUAL_DIFF_SKIPPED` finding (shown on the reports, logged in the chain).

use crate::analyzer::{DomainOptions, Finding, Severity};
use crate::hash::{HashAlgo, hash_bytes};
use crate::pdfscan::Writer;
use rv_pdfauth_pdf::PdfReport;
use rv_pdfauth_render::{DiffParams, Renderer, compose, diff, encode_png};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::OnceLock;

/// The PDFium build this release is tested with (third_party/pdfium/PDFIUM_VERSION.txt).
const PDFIUM_PIN: &str = include_str!("../../../third_party/pdfium/PDFIUM_VERSION.txt");

fn pin(key: &str) -> Option<&'static str> {
    PDFIUM_PIN.lines().find_map(|l| {
        l.strip_prefix(key)
            .and_then(|r| r.strip_prefix('='))
            .map(str::trim)
    })
}

/// Item-level limitation code (worded in i18n `limitations`) when the visual comparison is missing in part or whole.
pub const UNAVAILABLE: &str = "VISUAL_DIFF_UNAVAILABLE";

pub struct VisualOutcome {
    pub details: Value,
    pub findings: Vec<Finding>,
}

/// Where PDFium is looked for: `RV_PDFIUM_PATH` (file or folder), then next to the executable.
pub fn pdfium_candidates() -> Vec<PathBuf> {
    let env = std::env::var_os("RV_PDFIUM_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    rv_pdfauth_render::library_candidates(env.as_deref(), exe.as_deref())
}

fn library_sha256(path: &std::path::Path) -> String {
    static CACHE: OnceLock<(PathBuf, String)> = OnceLock::new();
    let c = CACHE.get_or_init(|| {
        let h = std::fs::read(path)
            .map(|b| hash_bytes(&b, HashAlgo::Sha256))
            .unwrap_or_else(|e| format!("unreadable: {}", e));
        (path.to_path_buf(), h)
    });
    if c.0 == path {
        c.1.clone()
    } else {
        std::fs::read(path)
            .map(|b| hash_bytes(&b, HashAlgo::Sha256))
            .unwrap_or_default()
    }
}

fn pct(ppm: u64) -> String {
    format!("{}.{:04}%", ppm / 10_000, ppm % 10_000)
}

fn list(v: &[u32]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.iter().map(u32::to_string).collect::<Vec<_>>().join(",")
    }
}

fn skipped(reason: String, params: &DiffParams, opts: &DomainOptions) -> VisualOutcome {
    VisualOutcome {
        findings: vec![
            Finding::new("VISUAL_DIFF_SKIPPED", Severity::Info).with("reason", reason.clone()),
        ],
        details: json!({"status": "skipped", "reason": reason, "dpi": opts.dpi, "max_pages": opts.max_pages,
            "params": params, "limitation": UNAVAILABLE}),
    }
}

pub(crate) fn visual_diff(
    bytes: &[u8],
    report: &PdfReport,
    w: &mut Writer,
    opts: &DomainOptions,
) -> VisualOutcome {
    let params = DiffParams::default();
    let renderer = match Renderer::load(&pdfium_candidates()) {
        Ok(r) => r,
        Err(e) => return skipped(e, &params, opts),
    };
    let renderer_info = json!({
        "library": renderer.library.display().to_string(),
        "sha256": library_sha256(&renderer.library),
        "pinned_tag": pin("tag"),
        "pinned_version": pin("version"),
        "bindings": pin("pdfium_render_feature"),
        "rendering": "CPU (PDFium, no GPU)",
    });
    let mut findings = Vec::new();
    let mut pairs = Vec::new();
    let mut pages_changed_total = 0u64;
    let n = report.revisions.len();
    for to in 2..=n {
        let from = to - 1;
        let mut pair = json!({"from_rev": from, "to_rev": to});
        let (Some(a), Some(b)) = (
            rv_pdfauth_pdf::revision_bytes(bytes, report, from),
            rv_pdfauth_pdf::revision_bytes(bytes, report, to),
        ) else {
            pair["status"] = json!("error");
            pair["reason"] = json!("revision byte range outside the file");
            pairs.push(pair);
            continue;
        };
        let docs = renderer
            .open(a)
            .and_then(|da| renderer.open(b).map(|db| (da, db)));
        let (da, db) = match docs {
            Ok(d) => d,
            Err(e) => {
                findings.push(
                    Finding::new("VISUAL_DIFF_SKIPPED", Severity::Info)
                        .with("reason", format!("revisions {}→{}: {}", from, to, e)),
                );
                pair["status"] = json!("error");
                pair["reason"] = json!(e);
                pairs.push(pair);
                continue;
            }
        };
        let (pa, pb) = (da.page_count(), db.page_count());
        let common = pa.min(pb);
        let compared = common.min(opts.max_pages);
        let added: Vec<u32> = (pa + 1..=pb).collect();
        let removed: Vec<u32> = (pb + 1..=pa).collect();
        let mut pages = Vec::new();
        let mut changed_pages = Vec::new();
        let mut max_ppm = 0u64;
        for p in 0..compared {
            let pno = p + 1;
            let ra = da.render_page(p, opts.dpi);
            let rb = db.render_page(p, opts.dpi);
            let ((ia, dpi_a), (ib, dpi_b)) = match (ra, rb) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => {
                    pages.push(json!({"page": pno, "status": "error", "reason": e}));
                    continue;
                }
            };
            let d = diff(&ia, &ib, &params);
            let mut entry = json!({"page": pno, "status": "compared", "changed": d.changed(),
                "changed_ratio": d.changed_ratio(), "changed_ppm": d.changed_ppm(),
                "changed_pixels": d.changed_pixels, "dpi": dpi_a.min(dpi_b),
                "width": d.width, "height": d.height, "size_changed": d.size_changed,
                "boxes": d.boxes, "boxes_truncated": d.boxes_truncated});
            if d.changed() {
                changed_pages.push(pno);
                max_ppm = max_ppm.max(d.changed_ppm());
                let caption = format!(
                    "REVISION {} VS {} - PAGE {} - CHANGED {} - {} REGION{}{}",
                    from,
                    to,
                    pno,
                    pct(d.changed_ppm()),
                    d.boxes.len(),
                    if d.boxes.len() == 1 { "" } else { "S" },
                    if d.size_changed {
                        " - PAGE SIZE CHANGED"
                    } else {
                        ""
                    }
                );
                let img = compose(&ia, &ib, &d.boxes, &caption);
                let rel = format!("visual/rev-{:02}-vs-{:02}-page-{:03}.png", from, to, pno);
                match encode_png(&img) {
                    Ok(png) => {
                        if w.write(&rel, "visual_diff", &png) {
                            entry["png"] = json!(rel);
                        }
                    }
                    Err(e) => entry["png_error"] = json!(e),
                }
            }
            pages.push(entry);
        }
        pages_changed_total += changed_pages.len() as u64;
        pair["status"] = json!("done");
        pair["pages_from"] = json!(pa);
        pair["pages_to"] = json!(pb);
        pair["pages_added"] = json!(added);
        pair["pages_removed"] = json!(removed);
        pair["pages_compared"] = json!(compared);
        pair["pages_beyond_cap"] = json!(common - compared);
        pair["pages_changed"] = json!(changed_pages);
        pair["changed_ratio_max"] = json!(rv_pdfauth_render::ratio_string(max_ppm));
        pair["pages"] = Value::Array(pages);
        if !changed_pages.is_empty() || !added.is_empty() || !removed.is_empty() {
            let mut f = Finding::new("VISUAL_CHANGE_BETWEEN_REVISIONS", Severity::Medium)
                .with("from_rev", from.to_string())
                .with("to_rev", to.to_string())
                .with("pages", list(&changed_pages))
                .with(
                    "changed_ratio_max",
                    rv_pdfauth_render::ratio_string(max_ppm),
                )
                .with("pages_added", list(&added))
                .with("pages_removed", list(&removed));
            if common > compared {
                f = f.with("pages_not_compared", format!("{}-{}", compared + 1, common));
            }
            findings.push(f);
        }
        pairs.push(pair);
    }
    // A revision PDFium could not open, or a page it could not render, leaves part of the item without a
    // visual comparison: the item's reports then print the VISUAL_DIFF_UNAVAILABLE limitation.
    let incomplete = pairs.iter().any(|p| {
        p["status"] == "error"
            || p["pages"]
                .as_array()
                .is_some_and(|pages| pages.iter().any(|x| x["status"] == "error"))
    });
    let mut details = json!({"status": "done", "renderer": renderer_info, "dpi": opts.dpi,
        "max_pages": opts.max_pages, "params": params, "pairs_compared": pairs.len(),
        "pages_changed": pages_changed_total, "pairs": pairs});
    if incomplete {
        details["limitation"] = json!(UNAVAILABLE);
    }
    VisualOutcome { findings, details }
}
