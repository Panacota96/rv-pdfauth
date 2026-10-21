//! Renderer of RV PDFAuth: page rendering with PDFium and the pixel diff between two revisions.
//!
//! * [`Renderer`] binds a PDFium shared library **at run time** (never linked): `pdfium.dll` / `libpdfium.so`
//!   next to the executable, or a path given by the caller. Missing library = an `Err` with the places
//!   tried, never a crash, so the caller can skip the visual diff and say why.
//! * [`diff`] compares two RGB renders: per-pixel tolerance (anti-aliasing), small noise dropped,
//!   changed regions grouped into padded, merged bounding boxes.
//! * [`compose`] draws previous | current side by side with the boxes outlined in Rakvor violet and a
//!   caption strip; [`encode_png`] writes it.
//!
//! Nothing here touches the evidence file or the audit log: the core passes revision bytes in and
//! writes (and hashes) whatever comes out.

mod font;
mod image;

pub use image::{
    BRAND_INK, BRAND_LIGHT, BRAND_PANEL, BRAND_VIOLET, DiffBox, DiffParams, DiffResult, RgbImage,
    compose, diff, encode_png, ratio_string,
};

use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Largest render we produce for one page (pixels). Above this the DPI is lowered for that page and the
/// effective DPI is reported: a 2 m × 1 m plan at 110 DPI would otherwise need gigabytes.
pub const MAX_PAGE_PIXELS: u64 = 40_000_000;

/// Library file name on this platform (`pdfium.dll`, `libpdfium.so`, `libpdfium.dylib`).
pub fn library_file_name() -> std::ffi::OsString {
    Pdfium::pdfium_platform_library_name()
}

/// Where to look for PDFium, in order: the explicit override (file, or folder holding the library), then
/// the folder of the running executable and its parent (test binaries live in `target/<profile>/deps`).
/// The current working directory is deliberately **not** searched (DLL planting next to evidence).
pub fn library_candidates(override_path: Option<&Path>, exe: Option<&Path>) -> Vec<PathBuf> {
    let name = library_file_name();
    let mut out = Vec::new();
    if let Some(p) = override_path {
        if p.is_dir() {
            out.push(p.join(&name));
        } else {
            out.push(p.to_path_buf());
        }
    }
    if let Some(dir) = exe.and_then(Path::parent) {
        out.push(dir.join(&name));
        if let Some(up) = dir.parent() {
            out.push(up.join(&name));
        }
    }
    out
}

/// Library path bound in this process: PDFium bindings are process-global and can be bound only once.
static BOUND: Mutex<Option<PathBuf>> = Mutex::new(None);

pub struct Renderer {
    pdfium: Pdfium,
    /// The PDFium library actually loaded (hash it for the audit trail).
    pub library: PathBuf,
}

impl Renderer {
    /// Binds the first candidate that exists and exports every function the bindings need.
    pub fn load(candidates: &[PathBuf]) -> Result<Renderer, String> {
        let mut bound = BOUND.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lib) = bound.as_ref() {
            // Already bound by an earlier call: Pdfium::default() re-uses the global bindings.
            return Ok(Renderer {
                pdfium: Pdfium::default(),
                library: lib.clone(),
            });
        }
        let mut tried = Vec::new();
        for c in candidates {
            if !c.is_file() {
                tried.push(format!("{} (not found)", c.display()));
                continue;
            }
            match Pdfium::bind_to_library(c) {
                Ok(bindings) => {
                    *bound = Some(c.clone());
                    return Ok(Renderer {
                        pdfium: Pdfium::new(bindings),
                        library: c.clone(),
                    });
                }
                Err(e) => tried.push(format!("{} ({})", c.display(), e)),
            }
        }
        Err(if tried.is_empty() {
            "PDFium library not found: no location to search".into()
        } else {
            format!(
                "PDFium library not found or not loadable: {}",
                tried.join("; ")
            )
        })
    }

    /// Opens one revision (bytes = file prefix up to that revision's %%EOF line). No password is tried.
    pub fn open<'a>(&'a self, bytes: &'a [u8]) -> Result<Doc<'a>, String> {
        self.pdfium
            .load_pdf_from_byte_slice(bytes, None)
            .map(|doc| Doc { doc })
            .map_err(|e| format!("PDFium cannot open this revision: {}", e))
    }
}

pub struct Doc<'a> {
    doc: PdfDocument<'a>,
}

impl Doc<'_> {
    pub fn page_count(&self) -> u32 {
        u32::try_from(self.doc.pages().len()).unwrap_or(0)
    }

    /// Renders page `index` (0-based) at `dpi` on a white background, CPU only. Returns the image and the
    /// DPI actually used (lower than asked when the page would exceed [`MAX_PAGE_PIXELS`]).
    pub fn render_page(&self, index: u32, dpi: u32) -> Result<(RgbImage, u32), String> {
        let idx = PdfPageIndex::try_from(index)
            .map_err(|_| format!("page {} out of range", index + 1))?;
        let page = self
            .doc
            .pages()
            .get(idx)
            .map_err(|e| format!("page {}: {}", index + 1, e))?;
        let (w_pt, h_pt) = (page.width().value.abs(), page.height().value.abs());
        let mut dpi = dpi.clamp(18, 600);
        let px = |d: u32| -> (u64, u64) {
            (
                ((w_pt * d as f32 / 72.0).ceil() as u64).max(1),
                ((h_pt * d as f32 / 72.0).ceil() as u64).max(1),
            )
        };
        while dpi > 18 {
            let (w, h) = px(dpi);
            if w * h <= MAX_PAGE_PIXELS && w <= 20_000 && h <= 20_000 {
                break;
            }
            dpi = (dpi * 3 / 4).max(18);
        }
        let (w, h) = px(dpi);
        let cfg = PdfRenderConfig::new()
            .set_target_width(w as i32)
            .set_target_height(h as i32)
            .set_format(PdfBitmapFormat::BGRA)
            .set_clear_color(PdfColor::WHITE)
            .render_form_data(true)
            .render_annotations(true);
        let bitmap = page
            .render_with_config(&cfg)
            .map_err(|e| format!("page {}: render failed: {}", index + 1, e))?;
        let (bw, bh) = (bitmap.width() as u32, bitmap.height() as u32);
        let rgba = bitmap.as_rgba_bytes();
        if bw == 0 || bh == 0 || rgba.len() < (bw as usize) * (bh as usize) * 4 {
            return Err(format!("page {}: empty bitmap", index + 1));
        }
        let stride = rgba.len() / bh as usize;
        let mut data = Vec::with_capacity(bw as usize * bh as usize * 3);
        for y in 0..bh as usize {
            let row = &rgba[y * stride..y * stride + bw as usize * 4];
            for p in row.chunks_exact(4) {
                // Composite over white in case the format carried alpha.
                let a = p[3] as u32;
                for &c in &p[..3] {
                    data.push(((c as u32 * a + 255 * (255 - a)) / 255) as u8);
                }
            }
        }
        Ok((
            RgbImage {
                width: bw,
                height: bh,
                data,
            },
            dpi,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/visual")
            .join(name);
        std::fs::read(p).unwrap()
    }

    /// PDFium from RV_PDFIUM_PATH (or next to the test binary). None = skip with a message.
    fn renderer() -> Option<Renderer> {
        let env = std::env::var_os("RV_PDFIUM_PATH").map(PathBuf::from);
        let exe = std::env::current_exe().ok();
        match Renderer::load(&library_candidates(env.as_deref(), exe.as_deref())) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!(
                    "skipping PDFium test (set RV_PDFIUM_PATH, see scripts/fetch_pdfium.sh): {e}"
                );
                None
            }
        }
    }

    #[test]
    fn missing_library_is_an_error_not_a_crash() {
        // Ok only if a parallel test already bound a real PDFium in this process (bindings are global).
        if let Err(e) = Renderer::load(&[PathBuf::from("/nonexistent/libpdfium.so")]) {
            assert!(e.contains("not found"), "{e}");
        }
        if let Err(e) = Renderer::load(&[]) {
            assert!(e.contains("no location"), "{e}");
        }
    }

    #[test]
    fn candidates_skip_the_working_directory() {
        let c = library_candidates(None, Some(Path::new("/opt/rv/bin/rv-pdfauth")));
        assert_eq!(c[0], Path::new("/opt/rv/bin").join(library_file_name()));
        assert_eq!(c[1], Path::new("/opt/rv").join(library_file_name()));
        assert!(c.iter().all(|p| p.is_absolute()));
    }

    #[test]
    fn pdfium_renders_both_revisions_and_the_diff_finds_the_change() {
        let Some(r) = renderer() else { return };
        let two = fixture("two-revisions.pdf");
        let one = fixture("one-revision.pdf");
        assert!(two.starts_with(&one));
        let d1 = r.open(&one).unwrap();
        let d2 = r.open(&two).unwrap();
        assert_eq!(d1.page_count(), 1);
        assert_eq!(d2.page_count(), 2);
        let (a, dpi) = d1.render_page(0, 110).unwrap();
        let (b, _) = d2.render_page(0, 110).unwrap();
        assert_eq!(dpi, 110);
        assert_eq!((a.width, a.height), (459, 306)); // 300 × 200 pt at 110 DPI
        assert!(a.data.iter().any(|&v| v < 128), "text must be drawn");
        let same = diff(&a, &a, &DiffParams::default());
        assert_eq!(same.changed_pixels, 0);
        let res = diff(&a, &b, &DiffParams::default());
        assert!(res.changed_pixels > 0 && !res.boxes.is_empty());
        // The change is in the lower half (the total line and the new rectangle), not in the title.
        assert!(res.boxes.iter().all(|bx| bx.y > 40), "{:?}", res.boxes);
        let img = compose(&a, &b, &res.boxes, "REVISION 1 VS 2 - PAGE 1");
        assert!(encode_png(&img).unwrap().starts_with(b"\x89PNG"));
    }

    #[test]
    fn pdfium_refuses_garbage_without_crashing() {
        let Some(r) = renderer() else { return };
        assert!(r.open(b"%PDF-1.7\nthis is not a pdf").is_err());
    }
}
