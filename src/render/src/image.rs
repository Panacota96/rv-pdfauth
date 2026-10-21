//! Pixel diff, side-by-side composition and PNG encoding (pure Rust, no PDFium needed: unit-testable).

use serde::Serialize;

pub const BRAND_INK: [u8; 3] = [0x0D, 0x11, 0x17];
pub const BRAND_PANEL: [u8; 3] = [0x1A, 0x1F, 0x26];
pub const BRAND_VIOLET: [u8; 3] = [0x6B, 0x00, 0xFF];
pub const BRAND_LIGHT: [u8; 3] = [0xE6, 0xE6, 0xE6];
const BRAND_CYAN: [u8; 3] = [0x00, 0xE5, 0xFF];
const WHITE: [u8; 3] = [255, 255, 255];

/// 8-bit RGB, row-major, no padding.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbImage {
    pub fn new(width: u32, height: u32, fill: [u8; 3]) -> RgbImage {
        let mut data = Vec::with_capacity(width as usize * height as usize * 3);
        for _ in 0..(width as usize * height as usize) {
            data.extend_from_slice(&fill);
        }
        RgbImage {
            width,
            height,
            data,
        }
    }

    /// Pixel or white outside the image (pages are compared on a white canvas).
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> [u8; 3] {
        if x >= self.width || y >= self.height {
            return WHITE;
        }
        let i = (y as usize * self.width as usize + x as usize) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    #[inline]
    pub fn put(&mut self, x: u32, y: u32, c: [u8; 3]) {
        if x < self.width && y < self.height {
            let i = (y as usize * self.width as usize + x as usize) * 3;
            self.data[i..i + 3].copy_from_slice(&c);
        }
    }

    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, c: [u8; 3]) {
        for yy in y..y.saturating_add(h).min(self.height) {
            for xx in x..x.saturating_add(w).min(self.width) {
                self.put(xx, yy, c);
            }
        }
    }

    pub fn blit(&mut self, src: &RgbImage, x0: u32, y0: u32) {
        for y in 0..src.height {
            let ty = y0 + y;
            if ty >= self.height {
                break;
            }
            let w = src.width.min(self.width.saturating_sub(x0)) as usize;
            let s = y as usize * src.width as usize * 3;
            let d = (ty as usize * self.width as usize + x0 as usize) * 3;
            self.data[d..d + w * 3].copy_from_slice(&src.data[s..s + w * 3]);
        }
    }

    /// Rectangle outline of thickness `t` drawn just **outside** the box, so the changed pixels stay visible.
    pub fn outline(&mut self, b: &DiffBox, t: u32, c: [u8; 3], x_off: u32, y_off: u32) {
        let x0 = (b.x + x_off).saturating_sub(t);
        let y0 = (b.y + y_off).saturating_sub(t);
        let x1 = b.x + x_off + b.w; // exclusive
        let y1 = b.y + y_off + b.h;
        let ow = x1 + t - x0;
        self.fill_rect(x0, y0, ow, t, c);
        self.fill_rect(x0, y1, ow, t, c);
        self.fill_rect(x0, y0, t, y1 + t - y0, c);
        self.fill_rect(x1, y0, t, y1 + t - y0, c);
    }
}

/// Tuning of [`diff`]. Defaults are documented in docs/architecture.md (visual diff).
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DiffParams {
    /// A pixel is changed when any channel differs by more than this (0–255). Absorbs anti-aliasing and
    /// colour-rounding differences; identical content renders identically with the same PDFium build.
    pub tolerance: u8,
    /// Grid cell (px) used to group changed pixels.
    pub cell: u32,
    /// Changed areas closer than this (px) are merged into one region (letters of one word, one line).
    pub merge_gap: u32,
    /// Padding (px) added around every region.
    pub pad: u32,
    /// Regions with fewer changed pixels are ignored as rendering noise.
    pub min_pixels: u32,
    /// At most this many boxes are kept (largest first); the rest is reported as truncated.
    pub max_boxes: usize,
}

impl Default for DiffParams {
    fn default() -> Self {
        DiffParams {
            tolerance: 32,
            cell: 8,
            merge_gap: 12,
            pad: 4,
            min_pixels: 3,
            max_boxes: 100,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct DiffBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl DiffBox {
    fn x1(&self) -> u32 {
        self.x + self.w
    }
    fn y1(&self) -> u32 {
        self.y + self.h
    }
    fn touches(&self, o: &DiffBox, gap: u32) -> bool {
        self.x <= o.x1() + gap
            && o.x <= self.x1() + gap
            && self.y <= o.y1() + gap
            && o.y <= self.y1() + gap
    }
    fn union(&self, o: &DiffBox) -> DiffBox {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        DiffBox {
            x,
            y,
            w: self.x1().max(o.x1()) - x,
            h: self.y1().max(o.y1()) - y,
        }
    }
    fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct DiffResult {
    /// Canvas compared (the larger of both renders in each dimension).
    pub width: u32,
    pub height: u32,
    /// Changed pixels that belong to a reported region (noise below `min_pixels` excluded).
    pub changed_pixels: u64,
    pub total_pixels: u64,
    pub boxes: Vec<DiffBox>,
    pub boxes_truncated: bool,
    /// The two renders do not have the same size (page size or rotation changed).
    pub size_changed: bool,
}

impl DiffResult {
    pub fn changed(&self) -> bool {
        self.changed_pixels > 0 || self.size_changed
    }
    /// Changed share in parts per million (integer: no floats in audited JSON).
    pub fn changed_ppm(&self) -> u64 {
        if self.total_pixels == 0 {
            return 0;
        }
        (self.changed_pixels as u128 * 1_000_000 / self.total_pixels as u128) as u64
    }
    /// Changed share as a decimal string with 6 places, e.g. "0.012345".
    pub fn changed_ratio(&self) -> String {
        ratio_string(self.changed_ppm())
    }
}

/// "0.012345" from parts per million.
pub fn ratio_string(ppm: u64) -> String {
    format!("{}.{:06}", ppm / 1_000_000, ppm % 1_000_000)
}

/// Compares two renders of the same page. Outside an image the canvas is white.
pub fn diff(a: &RgbImage, b: &RgbImage, p: &DiffParams) -> DiffResult {
    let w = a.width.max(b.width);
    let h = a.height.max(b.height);
    let cell = p.cell.max(1);
    let (cw, ch) = (w.div_ceil(cell) as usize, h.div_ceil(cell) as usize);
    // Per cell: changed count and tight bounds (min x, min y, max x, max y).
    let mut count = vec![0u32; cw * ch];
    let mut bounds = vec![(u32::MAX, u32::MAX, 0u32, 0u32); cw * ch];
    let tol = p.tolerance as i16;
    for y in 0..h {
        for x in 0..w {
            let (pa, pb) = (a.get(x, y), b.get(x, y));
            let changed = (0..3).any(|i| (pa[i] as i16 - pb[i] as i16).abs() > tol);
            if changed {
                let c = (y / cell) as usize * cw + (x / cell) as usize;
                count[c] += 1;
                let bd = &mut bounds[c];
                bd.0 = bd.0.min(x);
                bd.1 = bd.1.min(y);
                bd.2 = bd.2.max(x);
                bd.3 = bd.3.max(y);
            }
        }
    }
    // Connected components over changed cells; neighbours within the merge gap are connected.
    let reach = 1 + (p.merge_gap / cell) as isize;
    let mut seen = vec![false; cw * ch];
    let mut regions: Vec<(DiffBox, u64)> = Vec::new();
    let mut stack = Vec::new();
    for start in 0..cw * ch {
        if count[start] == 0 || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let mut bx: Option<DiffBox> = None;
        let mut pixels = 0u64;
        while let Some(c) = stack.pop() {
            pixels += count[c] as u64;
            let bd = bounds[c];
            let cb = DiffBox {
                x: bd.0,
                y: bd.1,
                w: bd.2 - bd.0 + 1,
                h: bd.3 - bd.1 + 1,
            };
            bx = Some(bx.map_or(cb, |b| b.union(&cb)));
            let (cx, cy) = ((c % cw) as isize, (c / cw) as isize);
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    let (nx, ny) = (cx + dx, cy + dy);
                    if nx < 0 || ny < 0 || nx >= cw as isize || ny >= ch as isize {
                        continue;
                    }
                    let n = ny as usize * cw + nx as usize;
                    if count[n] > 0 && !seen[n] {
                        seen[n] = true;
                        stack.push(n);
                    }
                }
            }
        }
        if let Some(b) = bx {
            if pixels >= p.min_pixels as u64 {
                regions.push((b, pixels));
            }
        }
    }
    let changed_pixels = regions.iter().map(|r| r.1).sum();
    // Pad, clamp, then merge boxes that overlap after padding until stable.
    let mut boxes: Vec<DiffBox> = regions
        .iter()
        .map(|(b, _)| {
            let x = b.x.saturating_sub(p.pad);
            let y = b.y.saturating_sub(p.pad);
            DiffBox {
                x,
                y,
                w: (b.x1() + p.pad).min(w) - x,
                h: (b.y1() + p.pad).min(h) - y,
            }
        })
        .collect();
    loop {
        let mut merged = false;
        let mut i = 0;
        while i < boxes.len() {
            let mut j = i + 1;
            while j < boxes.len() {
                if boxes[i].touches(&boxes[j], 0) {
                    let o = boxes.swap_remove(j);
                    boxes[i] = boxes[i].union(&o);
                    merged = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
        if !merged {
            break;
        }
    }
    let mut boxes_truncated = false;
    if boxes.len() > p.max_boxes {
        boxes.sort_by_key(|b| std::cmp::Reverse(b.area()));
        boxes.truncate(p.max_boxes);
        boxes_truncated = true;
    }
    boxes.sort_by_key(|b| (b.y, b.x));
    DiffResult {
        width: w,
        height: h,
        changed_pixels,
        total_pixels: w as u64 * h as u64,
        boxes,
        boxes_truncated,
        size_changed: a.width != b.width || a.height != b.height,
    }
}

const MARGIN: u32 = 12;
const GUTTER: u32 = 16;
const SCALE: u32 = 2;
const LINE: u32 = 8 * SCALE + 4;
const STRIP: u32 = 10 + 2 * LINE + 6;

/// previous | current on the dark panel, changed boxes outlined in violet on **both** halves, caption strip on
/// top (`caption` on line 1, the half labels on line 2). Text is drawn with a built-in 5×7 font (ASCII).
pub fn compose(prev: &RgbImage, curr: &RgbImage, boxes: &[DiffBox], caption: &str) -> RgbImage {
    let ih = prev.height.max(curr.height);
    let width = MARGIN + prev.width + GUTTER + curr.width + MARGIN;
    let height = STRIP + ih + MARGIN;
    let mut img = RgbImage::new(width, height, BRAND_PANEL);
    img.fill_rect(0, 0, width, STRIP - 4, BRAND_INK);
    img.fill_rect(0, STRIP - 4, width, 2, BRAND_VIOLET);
    img.fill_rect(0, 8, 4, LINE * 2, BRAND_VIOLET);
    crate::font::draw_text(&mut img, MARGIN, 10, caption, BRAND_LIGHT, SCALE);
    let xl = MARGIN;
    let xr = MARGIN + prev.width + GUTTER;
    crate::font::draw_text(&mut img, xl, 10 + LINE, "PREVIOUS", BRAND_CYAN, SCALE);
    crate::font::draw_text(&mut img, xr, 10 + LINE, "CURRENT", BRAND_CYAN, SCALE);
    img.blit(prev, xl, STRIP);
    img.blit(curr, xr, STRIP);
    for b in boxes {
        img.outline(b, 3, BRAND_VIOLET, xl, STRIP);
        img.outline(b, 3, BRAND_VIOLET, xr, STRIP);
    }
    img
}

/// PNG (8-bit RGB) bytes.
pub fn encode_png(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Balanced);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&img.data).map_err(|e| e.to_string())?;
        w.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(w: u32, h: u32) -> RgbImage {
        RgbImage::new(w, h, WHITE)
    }

    #[test]
    fn identical_images_have_no_change() {
        let mut a = page(200, 100);
        a.fill_rect(10, 10, 50, 20, [0, 0, 0]);
        let r = diff(&a, &a.clone(), &DiffParams::default());
        assert_eq!(r.changed_pixels, 0);
        assert!(r.boxes.is_empty() && !r.changed());
        assert_eq!(r.changed_ratio(), "0.000000");
    }

    #[test]
    fn one_changed_rectangle_gives_one_padded_box() {
        let a = page(200, 100);
        let mut b = a.clone();
        b.fill_rect(50, 40, 30, 10, [0, 0, 0]);
        let r = diff(&a, &b, &DiffParams::default());
        assert_eq!(r.changed_pixels, 300);
        assert_eq!(
            r.boxes,
            vec![DiffBox {
                x: 46,
                y: 36,
                w: 38,
                h: 18
            }]
        );
        assert_eq!(r.changed_ppm(), 15_000); // 300 / 20 000
        assert_eq!(r.changed_ratio(), "0.015000");
    }

    #[test]
    fn near_changes_merge_far_changes_stay_apart() {
        let a = page(400, 200);
        let mut b = a.clone();
        // Two "letters" 6 px apart: one word, one box.
        b.fill_rect(20, 20, 8, 12, [0, 0, 0]);
        b.fill_rect(34, 20, 8, 12, [0, 0, 0]);
        // Far away: a second box.
        b.fill_rect(300, 150, 10, 10, [0, 0, 0]);
        let r = diff(&a, &b, &DiffParams::default());
        assert_eq!(r.boxes.len(), 2, "{:?}", r.boxes);
        assert!(r.boxes[0].x <= 20 && r.boxes[0].x1() >= 42);
        assert!(r.boxes[1].x <= 300 && r.boxes[1].y <= 150);
    }

    #[test]
    fn anti_aliasing_and_single_pixel_noise_are_ignored() {
        let mut a = page(100, 100);
        a.fill_rect(10, 10, 20, 20, [120, 120, 120]);
        let mut b = a.clone();
        // Edge shading within tolerance (anti-aliasing / rounding).
        b.fill_rect(10, 10, 20, 1, [140, 140, 140]);
        // One isolated pixel above tolerance (< min_pixels).
        b.put(80, 80, [0, 0, 0]);
        let r = diff(&a, &b, &DiffParams::default());
        assert_eq!(r.changed_pixels, 0, "{:?}", r);
        assert!(r.boxes.is_empty());
    }

    #[test]
    fn size_change_is_reported() {
        let a = page(100, 100);
        let b = page(100, 120);
        let r = diff(&a, &b, &DiffParams::default());
        assert!(r.size_changed && r.changed());
        assert_eq!((r.width, r.height), (100, 120));
        // White on white: no pixel changed, only the size.
        assert_eq!(r.changed_pixels, 0);
        let mut c = page(100, 120);
        c.fill_rect(0, 110, 100, 10, [0, 0, 0]);
        let r = diff(&a, &c, &DiffParams::default());
        assert_eq!(r.changed_pixels, 1000);
        assert_eq!(r.boxes[0].y1(), 120);
    }

    #[test]
    fn too_many_boxes_are_truncated_largest_first() {
        let a = page(400, 400);
        let mut b = a.clone();
        for i in 0..10 {
            b.fill_rect(i * 40, 0, 2 + i, 2 + i, [0, 0, 0]);
        }
        let p = DiffParams {
            max_boxes: 3,
            ..DiffParams::default()
        };
        let r = diff(&a, &b, &p);
        assert!(r.boxes_truncated);
        assert_eq!(r.boxes.len(), 3);
        // Largest three = rectangles 7, 8, 9 (x 280, 320, 360; boxes padded by 4).
        assert_eq!(
            r.boxes.iter().map(|bx| bx.x).collect::<Vec<_>>(),
            [276, 316, 356]
        );
    }

    #[test]
    fn compose_outlines_both_halves_and_encodes_png() {
        let a = page(100, 80);
        let mut b = a.clone();
        b.fill_rect(40, 30, 10, 10, [0, 0, 0]);
        let r = diff(&a, &b, &DiffParams::default());
        let img = compose(
            &a,
            &b,
            &r.boxes,
            "REVISION 1 VS 2 - PAGE 1 - CHANGED 0.0125%",
        );
        assert_eq!(img.width, MARGIN + 100 + GUTTER + 100 + MARGIN);
        assert_eq!(img.height, STRIP + 80 + MARGIN);
        let bx = r.boxes[0];
        // Outline pixel just left of the box, on both halves.
        assert_eq!(img.get(MARGIN + bx.x - 1, STRIP + bx.y + 2), BRAND_VIOLET);
        let xr = MARGIN + 100 + GUTTER;
        assert_eq!(img.get(xr + bx.x - 1, STRIP + bx.y + 2), BRAND_VIOLET);
        // The changed pixels themselves are not painted over.
        assert_eq!(img.get(xr + 45, STRIP + 35), [0, 0, 0]);
        // Caption text drawn in light colour.
        assert!((0..img.width).any(|x| (10..10 + 14).any(|y| img.get(x, y) == BRAND_LIGHT)));
        let png_bytes = encode_png(&img).unwrap();
        let dec = png::Decoder::new(std::io::Cursor::new(png_bytes));
        let reader = dec.read_info().unwrap();
        assert_eq!(reader.info().width, img.width);
        assert_eq!(reader.info().height, img.height);
    }

    #[test]
    fn ratio_string_formats_ppm() {
        assert_eq!(ratio_string(0), "0.000000");
        assert_eq!(ratio_string(1), "0.000001");
        assert_eq!(ratio_string(1_000_000), "1.000000");
    }
}
