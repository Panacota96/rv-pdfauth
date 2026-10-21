//! Stable-toolchain stand-in for the cargo-fuzz target (fuzz/fuzz_targets/analyze.rs): feeds
//! 20 000 deterministic random mutations of the sample PDFs to `analyze()` and asserts that none
//! panics. Stack overflows would abort the test binary, so recursion limits are exercised too.

mod common;

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const TOKENS: &[&[u8]] = &[
    b"<<",
    b">>",
    b"[",
    b"]",
    b"(",
    b")",
    b"<",
    b">",
    b"/",
    b"%%EOF\n",
    b"startxref\n0\n",
    b"xref\n0 1\n",
    b" obj ",
    b"endobj",
    b"stream\n",
    b"endstream",
    b" 0 R ",
    b"trailer",
    b"/Prev 0",
    b"/XRefStm 9",
    b"/Type /ObjStm /N 9 /First 3",
    b"/Type /XRef /W [1 4 2]",
    b"/Filter /FlateDecode",
    b"/Length 99999",
    b"/DecodeParms << /Predictor 15 /Columns 9 >>",
    b"BT 3 Tr (x) Tj ET",
    b"q 612 0 0 792 0 0 cm /Im0 Do",
    b"/ByteRange [0 10 20 30]",
    b"/Contents <3082>",
    b"-99999999999999999999",
    b"1e400",
    b"\\",
    b"#",
];

fn mutate(rng: &mut XorShift, base: &[u8]) -> Vec<u8> {
    let mut v = base.to_vec();
    let n = 1 + rng.below(8);
    for _ in 0..n {
        let len = v.len().max(1);
        match rng.below(7) {
            0 => {
                let i = rng.below(len);
                if let Some(b) = v.get_mut(i) {
                    *b ^= 1 << rng.below(8);
                }
            }
            1 => {
                let i = rng.below(len);
                if let Some(b) = v.get_mut(i) {
                    *b = rng.next() as u8;
                }
            }
            2 => {
                let i = rng.below(v.len() + 1);
                let t = TOKENS[rng.below(TOKENS.len())];
                v.splice(i..i, t.iter().copied());
            }
            3 => {
                let a = rng.below(len);
                let b = (a + rng.below(64)).min(v.len());
                if a < b {
                    v.drain(a..b);
                }
            }
            4 => {
                let a = rng.below(len);
                let b = (a + rng.below(256)).min(v.len());
                let chunk: Vec<u8> = v.get(a..b).unwrap_or(&[]).to_vec();
                let i = rng.below(v.len() + 1);
                v.splice(i..i, chunk);
            }
            5 => {
                let cut = rng.below(len);
                v.truncate(cut);
            }
            _ => {
                // Rewrite a number to an extreme value.
                let i = rng.below(len);
                if let Some(p) = v.iter().skip(i).position(|c| c.is_ascii_digit()) {
                    let at = i + p;
                    let extreme: &[u8] = match rng.below(3) {
                        0 => b"0",
                        1 => b"4294967295",
                        _ => b"18446744073709551616",
                    };
                    v.splice(at..at + 1, extreme.iter().copied());
                }
            }
        }
    }
    v
}

#[test]
fn twenty_thousand_mutations_never_panic() {
    let names = [
        "original.pdf",
        "edited-incremental.pdf",
        "signed-then-modified.pdf",
        "online-editor.pdf",
        "broken-xref.pdf",
        "scan-with-text.pdf",
        "crash.pdf",
    ];
    let samples: Vec<Vec<u8>> = names.iter().map(|n| common::sample(n)).collect();
    let opts = rv_pdfauth_pdf::PdfOptions {
        object_tree: true,
        embedded: true,
        ..Default::default()
    };
    let iterations: usize = std::env::var("RV_MUTATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let mut rng = XorShift(0x5EED_CAFE_F00D_1234);
    for i in 0..iterations {
        // The large scanned sample is used less often to keep the test fast.
        let which = if i % 16 == 0 {
            5
        } else {
            [0, 1, 2, 3, 4, 6][rng.below(6)]
        };
        let input = mutate(&mut rng, &samples[which]);
        let res = std::panic::catch_unwind(|| rv_pdfauth_pdf::analyze(&input, &opts));
        if res.is_err() {
            let path = std::env::temp_dir().join(format!("rv-pdfauth-panic-{}.pdf", i));
            let _ = std::fs::write(&path, &input);
            panic!(
                "analyze() panicked on mutation {} of {} (saved to {})",
                i,
                names[which],
                path.display()
            );
        }
    }
}

#[test]
fn pathological_inputs_never_panic() {
    let opts = rv_pdfauth_pdf::PdfOptions::default();
    let mut cases: Vec<Vec<u8>> = vec![
        b"%PDF-".to_vec(),
        b"%PDF-1.7\n%%EOF".to_vec(),
        b"%PDF-1.7\nstartxref\n18446744073709551615\n%%EOF\n".to_vec(),
        b"%PDF-1.7\n1 0 obj\n<< /Length 1 0 R >>\nstream\nendstream\nendobj\nstartxref\n9\n%%EOF"
            .to_vec(),
    ];
    let mut deep = b"%PDF-1.7\n1 0 obj\n".to_vec();
    deep.extend(std::iter::repeat_n(b'[', 200_000));
    cases.push(deep);
    let mut dicts = b"%PDF-1.7\n1 0 obj\n".to_vec();
    dicts.extend(std::iter::repeat_n(&b"<< /A "[..], 50_000).flatten());
    cases.push(dicts);
    let mut eofs = b"%PDF-1.7\n".to_vec();
    eofs.extend(std::iter::repeat_n(&b"%%EOF\n"[..], 5_000).flatten());
    cases.push(eofs);
    let mut objs = b"%PDF-1.7\n".to_vec();
    objs.extend(std::iter::repeat_n(&b"1 0 obj (((((\n"[..], 20_000).flatten());
    cases.push(objs);
    for c in cases {
        let _ = rv_pdfauth_pdf::analyze(&c, &opts);
    }
}
