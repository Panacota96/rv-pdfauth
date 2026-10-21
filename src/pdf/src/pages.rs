//! Page tree, content-stream interpretation (text visibility, image coverage) and page content
//! signatures used to detect which pages changed between revisions.

use crate::doc::Doc;
use crate::obj::{Dict, Lexer, Obj, Parser, Ref, Tok};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

pub const MAX_PAGES: usize = 100_000;
const MAX_TREE_DEPTH: usize = 64;
const MAX_FORM_DEPTH: usize = 4;
const MAX_OPERANDS: usize = 256;
const MAX_GSTATE: usize = 64;

#[derive(Clone, Debug)]
pub struct Page {
    pub obj: Option<Ref>,
    pub dict: Dict,
    pub resources: Option<Obj>,
    pub mediabox: [f64; 4],
}

/// Leaf pages of revision `rev`, in document order.
pub fn pages(doc: &Doc, rev: usize) -> Vec<Page> {
    let mut out = Vec::new();
    let Some(cat) = doc.catalog(rev) else {
        return out;
    };
    let Some(root) = cat.get(b"Pages").cloned() else {
        return out;
    };
    let mut visited: HashSet<Ref> = HashSet::new();
    // (node, inherited resources, inherited mediabox, depth)
    let mut stack: Vec<(Obj, Option<Obj>, Option<Obj>, usize)> = vec![(root, None, None, 0)];
    while let Some((node, res, mbox, depth)) = stack.pop() {
        if out.len() >= MAX_PAGES {
            doc.warn(
                0,
                format!("more than {} pages; the rest is ignored", MAX_PAGES),
            );
            break;
        }
        if depth > MAX_TREE_DEPTH {
            doc.warn(0, "page tree too deep".into());
            continue;
        }
        let r = node.as_ref();
        if let Some(r) = r
            && !visited.insert(r)
        {
            doc.warn(0, format!("page tree loop at object {} {}", r.0, r.1));
            continue;
        }
        let Some(d) = doc.get_dict(rev, &node) else {
            continue;
        };
        let res = d.get(b"Resources").cloned().or(res);
        let mbox = d.get(b"MediaBox").cloned().or(mbox);
        let kids = d.get(b"Kids").and_then(|k| doc.deref(rev, k));
        let is_leaf = d.type_is(b"Page") || (!d.type_is(b"Pages") && kids.is_none());
        if is_leaf {
            let mb = mbox
                .as_ref()
                .and_then(|m| doc.deref(rev, m))
                .and_then(|m| rect(doc, rev, &m))
                .unwrap_or([0.0, 0.0, 612.0, 792.0]);
            out.push(Page {
                obj: r,
                dict: d,
                resources: res.and_then(|x| doc.deref(rev, &x)),
                mediabox: mb,
            });
            continue;
        }
        if let Some(Obj::Array(k)) = kids {
            for kid in k.into_iter().rev() {
                stack.push((kid, res.clone(), mbox.clone(), depth + 1));
            }
        }
    }
    out
}

fn rect(doc: &Doc, rev: usize, o: &Obj) -> Option<[f64; 4]> {
    let a = o.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let mut v = [0.0; 4];
    for (i, x) in a.iter().enumerate() {
        v[i] = doc.deref(rev, x)?.as_f64()?;
    }
    Some([
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ])
}

/// Decoded, concatenated content streams of a page.
pub fn page_content(doc: &Doc, rev: usize, page: &Page) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(c) = page.dict.get(b"Contents") else {
        return out;
    };
    let c = doc.deref(rev, c).unwrap_or(Obj::Null);
    let parts: Vec<Obj> = match c {
        Obj::Array(a) => a,
        other => vec![other],
    };
    for p in parts.iter().take(10_000) {
        if let Some(Obj::Stream(s)) = doc.deref(rev, p)
            && let Some(d) = doc.decode_stream(&s, "page content")
        {
            out.extend_from_slice(&d);
            out.push(b'\n');
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContentStats {
    /// Text-showing operators with at least one non-space byte.
    pub text_shows: usize,
    /// Of those, shown with render mode 3/7, white fill or zero fill alpha.
    pub invisible_shows: usize,
    pub images: usize,
    /// Largest fraction of the page area covered by one image (0..1).
    pub max_image_cover: f64,
}

#[derive(Clone, Copy)]
struct GState {
    ctm: [f64; 6],
    render_mode: i64,
    fill_white: bool,
    fill_alpha_zero: bool,
    fill_cs_n: usize,
}

fn mul(m: [f64; 6], n: [f64; 6]) -> [f64; 6] {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

pub struct Interp<'d, 'a> {
    doc: &'d Doc<'a>,
    rev: usize,
    budget: &'d Cell<u64>,
    mediabox: [f64; 4],
    pub stats: ContentStats,
}

impl<'d, 'a> Interp<'d, 'a> {
    pub fn new(doc: &'d Doc<'a>, rev: usize, budget: &'d Cell<u64>, mediabox: [f64; 4]) -> Self {
        Interp {
            doc,
            rev,
            budget,
            mediabox,
            stats: ContentStats::default(),
        }
    }

    fn cover(&mut self, ctm: &[f64; 6]) {
        let pts = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
        let mut x0 = f64::INFINITY;
        let mut y0 = f64::INFINITY;
        let mut x1 = f64::NEG_INFINITY;
        let mut y1 = f64::NEG_INFINITY;
        for (x, y) in pts {
            let tx = ctm[0] * x + ctm[2] * y + ctm[4];
            let ty = ctm[1] * x + ctm[3] * y + ctm[5];
            x0 = x0.min(tx);
            y0 = y0.min(ty);
            x1 = x1.max(tx);
            y1 = y1.max(ty);
        }
        let [mx0, my0, mx1, my1] = self.mediabox;
        let w = (x1.min(mx1) - x0.max(mx0)).max(0.0);
        let h = (y1.min(my1) - y0.max(my0)).max(0.0);
        let area = (mx1 - mx0) * (my1 - my0);
        self.stats.images += 1;
        if area > 0.0 && (w * h).is_finite() {
            let c = (w * h / area).clamp(0.0, 1.0);
            if c > self.stats.max_image_cover {
                self.stats.max_image_cover = c;
            }
        }
    }

    /// Runs a content stream with `resources` (a resolved dictionary, or None).
    pub fn run(
        &mut self,
        data: &[u8],
        resources: Option<&Dict>,
        depth: usize,
        active: &mut Vec<Ref>,
    ) {
        let mut gs = GState {
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            render_mode: 0,
            fill_white: false,
            fill_alpha_zero: false,
            fill_cs_n: 1,
        };
        self.run_with(data, resources, depth, active, &mut gs);
    }

    fn res_entry(
        &self,
        resources: Option<&Dict>,
        cat: &[u8],
        name: &[u8],
    ) -> Option<(Obj, Option<Ref>)> {
        let sub = self.doc.get_dict(self.rev, resources?.get(cat)?)?;
        let v = sub.get(name)?;
        let r = v.as_ref();
        Some((self.doc.deref(self.rev, v)?, r))
    }

    fn run_with(
        &mut self,
        data: &[u8],
        resources: Option<&Dict>,
        depth: usize,
        active: &mut Vec<Ref>,
        gs: &mut GState,
    ) {
        let mut stack: Vec<GState> = Vec::new();
        let mut overflow = 0usize;
        let mut ops: Vec<Obj> = Vec::new();
        let mut p = Parser::new(data, 0);
        loop {
            let b = self.budget.get();
            if b == 0 {
                return;
            }
            self.budget.set(b - 1);
            let Some((_, tok)) = p.lex.next_tok() else {
                break;
            };
            let kw = match tok {
                Tok::Keyword(k) if !matches!(k.as_slice(), b"true" | b"false" | b"null") => k,
                Tok::ArrClose | Tok::DictClose => {
                    ops.clear();
                    continue;
                }
                t => {
                    let save = p.lex.pos;
                    match p.object_from(t, 1) {
                        Some(o) => {
                            if ops.len() < MAX_OPERANDS {
                                ops.push(o);
                            }
                        }
                        None => {
                            p.lex.pos = save.max(p.lex.pos);
                            ops.clear();
                        }
                    }
                    continue;
                }
            };
            let nums: Vec<f64> = ops.iter().filter_map(Obj::as_f64).collect();
            match kw.as_slice() {
                b"q" => {
                    if stack.len() < MAX_GSTATE {
                        stack.push(*gs);
                    } else {
                        overflow += 1;
                    }
                }
                b"Q" => {
                    if overflow > 0 {
                        overflow -= 1;
                    } else if let Some(s) = stack.pop() {
                        *gs = s;
                    }
                }
                b"cm" if nums.len() == 6 => {
                    let m = [nums[0], nums[1], nums[2], nums[3], nums[4], nums[5]];
                    if m.iter().all(|v| v.is_finite()) {
                        gs.ctm = mul(m, gs.ctm);
                    }
                }
                b"Tr" => {
                    if let Some(m) = ops.last().and_then(Obj::as_int) {
                        gs.render_mode = m;
                    }
                }
                b"g" if nums.len() == 1 => {
                    gs.fill_white = nums[0] >= 0.99;
                    gs.fill_cs_n = 1;
                }
                b"rg" if nums.len() == 3 => {
                    gs.fill_white = nums.iter().all(|v| *v >= 0.99);
                    gs.fill_cs_n = 3;
                }
                b"k" if nums.len() == 4 => {
                    gs.fill_white = nums.iter().all(|v| *v <= 0.01);
                    gs.fill_cs_n = 4;
                }
                b"cs" => {
                    gs.fill_cs_n = match ops.last().and_then(Obj::as_name) {
                        Some(b"DeviceGray") | Some(b"CalGray") | Some(b"G") => 1,
                        Some(b"DeviceRGB") | Some(b"CalRGB") | Some(b"RGB") => 3,
                        Some(b"DeviceCMYK") | Some(b"CMYK") => 4,
                        _ => 0,
                    };
                    gs.fill_white = false;
                }
                b"sc" | b"scn" => {
                    let n = if gs.fill_cs_n == 0 {
                        nums.len()
                    } else {
                        gs.fill_cs_n
                    };
                    gs.fill_white = nums.len() == n
                        && match n {
                            1 | 3 => nums.iter().all(|v| *v >= 0.99),
                            4 => nums.iter().all(|v| *v <= 0.01),
                            _ => false,
                        };
                }
                b"gs" => {
                    if let Some(name) = ops.last().and_then(Obj::as_name)
                        && let Some((Obj::Dict(d), _)) =
                            self.res_entry(resources, b"ExtGState", name)
                        && let Some(a) = d.get(b"ca").and_then(Obj::as_f64)
                    {
                        gs.fill_alpha_zero = a <= 0.001;
                    }
                }
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    let visible_bytes = |s: &[u8]| s.iter().any(|&c| c != b' ' && c != 0);
                    let has_text = match ops.last() {
                        Some(Obj::Str(s)) => visible_bytes(s),
                        Some(Obj::Array(a)) => {
                            a.iter().any(|o| o.as_str().is_some_and(visible_bytes))
                        }
                        _ => false,
                    };
                    if has_text {
                        self.stats.text_shows += 1;
                        let m = gs.render_mode;
                        let fills = matches!(m, 0 | 4);
                        if m == 3 || m == 7 || (fills && (gs.fill_white || gs.fill_alpha_zero)) {
                            self.stats.invisible_shows += 1;
                        }
                    }
                }
                b"Do" => {
                    if let Some(name) = ops.last().and_then(Obj::as_name)
                        && let Some((Obj::Stream(s), r)) =
                            self.res_entry(resources, b"XObject", name)
                    {
                        match s.dict.name(b"Subtype") {
                            Some(b"Image") => {
                                let ctm = gs.ctm;
                                self.cover(&ctm);
                            }
                            Some(b"Form") if depth < MAX_FORM_DEPTH => {
                                if let Some(r) = r {
                                    if active.contains(&r) {
                                        ops.clear();
                                        continue;
                                    }
                                    active.push(r);
                                }
                                if let Some(content) = self.doc.decode_stream(&s, "form XObject") {
                                    let fres = s
                                        .dict
                                        .get(b"Resources")
                                        .and_then(|x| self.doc.get_dict(self.rev, x));
                                    let mut inner = *gs;
                                    if let Some(m) = s.dict.get(b"Matrix").and_then(Obj::as_array) {
                                        let v: Vec<f64> =
                                            m.iter().filter_map(Obj::as_f64).collect();
                                        if v.len() == 6 && v.iter().all(|x| x.is_finite()) {
                                            inner.ctm = mul(
                                                [v[0], v[1], v[2], v[3], v[4], v[5]],
                                                inner.ctm,
                                            );
                                        }
                                    }
                                    let res = fres.as_ref().or(resources);
                                    self.run_with(&content, res, depth + 1, active, &mut inner);
                                }
                                if r.is_some() {
                                    active.pop();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                b"ID" => {
                    p.lex.skip_inline_image();
                    let ctm = gs.ctm;
                    self.cover(&ctm);
                }
                _ => {}
            }
            ops.clear();
        }
    }
}

/// Interprets the page content of revision `rev`.
pub fn page_stats(doc: &Doc, rev: usize, page: &Page, budget: &Cell<u64>) -> ContentStats {
    let data = page_content(doc, rev, page);
    let res = page.resources.as_ref().and_then(Obj::as_dict).cloned();
    let mut it = Interp::new(doc, rev, budget, page.mediabox);
    let mut active = Vec::new();
    it.run(&data, res.as_ref(), 0, &mut active);
    it.stats
}

/// Lexer check used by tests and the fuzzer: tokenizes a content stream to the end.
pub fn token_count(data: &[u8]) -> usize {
    let mut lx = Lexer::new(data, 0);
    let mut n = 0;
    while lx.next_tok().is_some() {
        n += 1;
    }
    n
}

// ------------------------------------------------------------------ content signatures

pub struct Hashing<'d, 'a> {
    doc: &'d Doc<'a>,
    rev: usize,
    memo: HashMap<Ref, u64>,
    in_progress: HashSet<Ref>,
}

impl<'d, 'a> Hashing<'d, 'a> {
    pub fn new(doc: &'d Doc<'a>, rev: usize) -> Self {
        Hashing {
            doc,
            rev,
            memo: HashMap::new(),
            in_progress: HashSet::new(),
        }
    }

    /// Deep hash of an object, following references (not their numbers), skipping /Parent.
    /// A reference hashes exactly like the object it points to, so moving an object between
    /// direct and indirect form does not change the hash.
    pub fn hash(&mut self, o: &Obj, depth: usize) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        let mut h = DefaultHasher::new();
        if depth > 12 {
            0xFFu8.hash(&mut h);
            return h.finish();
        }
        match o {
            Obj::Null => 0u8.hash(&mut h),
            Obj::Bool(b) => (1u8, *b).hash(&mut h),
            Obj::Int(i) => (2u8, *i).hash(&mut h),
            Obj::Real(r) => (3u8, r.to_bits()).hash(&mut h),
            Obj::Name(n) => (4u8, n).hash(&mut h),
            Obj::Str(s) => (5u8, s).hash(&mut h),
            Obj::Array(a) => {
                (6u8, a.len()).hash(&mut h);
                for x in a.iter().take(100_000) {
                    self.hash(x, depth + 1).hash(&mut h);
                }
            }
            Obj::Dict(d) => {
                7u8.hash(&mut h);
                self.dict_hash(d, depth).hash(&mut h);
            }
            Obj::Stream(s) => {
                8u8.hash(&mut h);
                self.dict_hash(&s.dict, depth).hash(&mut h);
                if let Some(raw) = self.doc.raw_stream(s) {
                    raw.hash(&mut h);
                }
            }
            Obj::Ref(n, g) => {
                let r = (*n, *g);
                if let Some(v) = self.memo.get(&r) {
                    return *v;
                }
                if self.in_progress.contains(&r) {
                    (10u8, *n, *g).hash(&mut h);
                    return h.finish();
                }
                self.in_progress.insert(r);
                let v = match self.doc.resolve(self.rev, r) {
                    Some(t) => self.hash(&t, depth + 1),
                    None => 0,
                };
                self.in_progress.remove(&r);
                self.memo.insert(r, v);
                return v;
            }
        }
        h.finish()
    }

    fn dict_hash(&mut self, d: &Dict, depth: usize) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut keys: Vec<&(Vec<u8>, Obj)> =
            d.0.iter()
                .filter(|(k, _)| k != b"Parent" && k != b"Length")
                .collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in keys {
            k.hash(&mut h);
            self.hash(v, depth + 1).hash(&mut h);
        }
        h.finish()
    }
}

/// Signature of what a page shows: its /Contents and /Resources (deep).
pub fn page_signature(hs: &mut Hashing, page: &Page) -> u64 {
    let c = page.dict.get(b"Contents").cloned().unwrap_or(Obj::Null);
    let r = page.resources.clone().unwrap_or(Obj::Null);
    let a = hs.hash(&c, 0);
    let b = hs.hash(&r, 0);
    a ^ b.rotate_left(17)
}
