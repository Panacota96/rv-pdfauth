//! File structure: linear scan (headers, objects, `startxref`, `%%EOF`), revisions, xref tables and
//! streams, trailers, object streams, and per-revision object resolution. Every fallback that a
//! reader would apply silently is recorded as a warning instead.

use crate::filters::{self, DecodeError};
use crate::obj::{
    self, Dict, IndirectObj, Lexer, Obj, ObjCtx, Ref, Tok, is_regular, parse_indirect,
    read_obj_header,
};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

/// Revisions beyond this are merged into the last one (hostile files with thousands of `%%EOF`).
pub const MAX_REVISIONS: usize = 2000;
/// At most this many parser warnings are kept per file (then one "further warnings suppressed" entry).
pub const MAX_WARNINGS: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum XEntry {
    Free,
    InUse { offset: u64, gen_: u16 },
    Compressed { stm: u32, idx: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XrefKind {
    Table,
    Stream,
    Hybrid,
    /// No usable xref section: objects come from the linear scan.
    Scan,
}

impl XrefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            XrefKind::Table => "table",
            XrefKind::Stream => "stream",
            XrefKind::Hybrid => "hybrid",
            XrefKind::Scan => "none",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Section {
    pub offset: usize,
    pub kind: XrefKind,
    pub entries: BTreeMap<u32, XEntry>,
    pub trailer: Dict,
}

#[derive(Clone, Debug, Default)]
pub struct ScanObj {
    pub num: u32,
    pub gen_: u16,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub headers: Vec<usize>,
    /// (offset of "%%EOF", end of its line).
    pub eofs: Vec<(usize, usize)>,
    /// (offset of "startxref", value).
    pub startxrefs: Vec<(usize, Option<u64>)>,
    pub xref_kws: Vec<usize>,
    pub objects: Vec<ScanObj>,
    pub endstreams: Vec<usize>,
    pub budget_exceeded: bool,
}

#[derive(Clone, Debug)]
pub struct RevInfo {
    pub start: usize,
    pub end: usize,
    pub eof_pos: usize,
    pub startxref: Option<u64>,
    pub section: Option<Section>,
    /// Trailer with /Root and /Info inherited from earlier revisions when missing.
    pub trailer: Dict,
    pub xref_consistent: bool,
    pub bad_entries: usize,
}

pub struct Doc<'a> {
    pub data: &'a [u8],
    pub scan: Scan,
    pub revs: Vec<RevInfo>,
    pub encrypted: bool,
    /// obj number → revisions (1-based, ascending) whose section lists it.
    by_num: HashMap<u32, Vec<usize>>,
    /// (num, gen) → scanned offsets, ascending.
    scan_index: HashMap<Ref, Vec<usize>>,
    file_cache: RefCell<HashMap<usize, Option<Rc<IndirectObj>>>>,
    objstm_cache: RefCell<HashMap<usize, Option<Rc<ObjStm>>>>,
    warnings: RefCell<Vec<(u64, String)>>,
    warned: RefCell<HashSet<String>>,
    parse_budget: Cell<u64>,
    decode_budget: Cell<u64>,
}

#[derive(Debug)]
pub struct ObjStm {
    /// (object number, parsed object) in stream order.
    pub objects: Vec<(u32, Option<Obj>)>,
}

/// Where a resolved object came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Loc {
    File(usize),
    /// Inside an object stream: (file offset of the object stream, its object number).
    InStream(usize, u32),
}

fn is_boundary(data: &[u8], i: usize) -> bool {
    i == 0 || data.get(i - 1).is_none_or(|&b| !is_regular(b))
}

fn word_at(data: &[u8], i: usize, w: &[u8]) -> bool {
    data.get(i..i + w.len()) == Some(w) && data.get(i + w.len()).is_none_or(|&b| !is_regular(b))
}

/// Linear scan of the whole file.
pub fn scan(data: &[u8]) -> Scan {
    let mut s = Scan::default();
    let mut i = 0usize;
    while let Some(p) = obj::find(data, b"endstream", i) {
        s.endstreams.push(p);
        i = p + 9;
    }
    let ctx = ObjCtx {
        resolve_len: &|_| None,
        endstreams: Some(&s.endstreams),
    };
    // Work budget for failed object parses (hostile files full of unterminated objects).
    let mut failed_work: u64 = 0;
    let budget = (data.len() as u64).saturating_mul(16) + (1 << 20);
    let mut objects = Vec::new();
    let mut i = 0usize;
    let n = data.len();
    while i < n {
        let b = data[i];
        match b {
            b'%' => {
                if data.get(i..i + 5) == Some(b"%PDF-") {
                    s.headers.push(i);
                }
                if data.get(i..i + 5) == Some(b"%%EOF") {
                    let mut j = i + 5;
                    while matches!(data.get(j), Some(b' ' | b'\t')) {
                        j += 1;
                    }
                    match (data.get(j), data.get(j + 1)) {
                        (Some(b'\r'), Some(b'\n')) => j += 2,
                        (Some(b'\r' | b'\n'), _) => j += 1,
                        _ => {
                            // No EOL: the marker ends right after "%%EOF".
                            j = i + 5;
                        }
                    }
                    s.eofs.push((i, j));
                    i = j;
                    continue;
                }
                // Comment: skip to end of line.
                while i < n && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'0'..=b'9' if is_boundary(data, i) && !s.budget_exceeded => {
                if let Some((num, gen_, after)) = read_obj_header(data, i) {
                    match parse_indirect(data, i, &ctx) {
                        Ok(o) => {
                            objects.push(ScanObj {
                                num,
                                gen_,
                                start: i,
                                end: o.end,
                            });
                            i = o.end.max(i + 1);
                            continue;
                        }
                        Err(reached) => {
                            failed_work += reached.saturating_sub(i) as u64;
                            if failed_work > budget {
                                s.budget_exceeded = true;
                            }
                            i = after;
                            continue;
                        }
                    }
                }
                // Skip the rest of this number token.
                while i < n && is_regular(data[i]) {
                    i += 1;
                }
            }
            b's' if is_boundary(data, i) && word_at(data, i, b"startxref") => {
                let mut lx = Lexer::new(data, i + 9);
                let v = match lx.next_tok() {
                    Some((_, Tok::Int(v))) if v >= 0 => Some(v as u64),
                    _ => None,
                };
                s.startxrefs.push((i, v));
                i += 9;
            }
            b'x' if is_boundary(data, i) && word_at(data, i, b"xref") => {
                s.xref_kws.push(i);
                i += 4;
            }
            _ => i += 1,
        }
    }
    s.objects = objects;
    s
}

impl<'a> Doc<'a> {
    pub fn open(data: &'a [u8]) -> Doc<'a> {
        let scan = scan(data);
        let mut scan_index: HashMap<Ref, Vec<usize>> = HashMap::new();
        for o in &scan.objects {
            scan_index.entry((o.num, o.gen_)).or_default().push(o.start);
        }
        let mut d = Doc {
            data,
            scan,
            revs: Vec::new(),
            encrypted: false,
            by_num: HashMap::new(),
            scan_index,
            file_cache: RefCell::new(HashMap::new()),
            objstm_cache: RefCell::new(HashMap::new()),
            warnings: RefCell::new(Vec::new()),
            warned: RefCell::new(HashSet::new()),
            parse_budget: Cell::new((data.len() as u64).saturating_mul(64) + (64 << 20)),
            decode_budget: Cell::new(2 << 30),
        };
        if d.scan.budget_exceeded {
            d.warn(
                0,
                "linear scan stopped parsing objects: too many malformed objects (work budget exceeded)"
                    .into(),
            );
        }
        for (i, h) in d.scan.headers.clone().iter().enumerate() {
            if i > 0 || *h > 0 {
                d.warn(*h as u64, format!("PDF header found at offset {}", h));
            }
        }
        d.build_revisions();
        d
    }

    pub fn warn(&self, offset: u64, msg: String) {
        let key = format!("{}:{}", offset, msg);
        let mut w = self.warned.borrow_mut();
        if w.contains(&key) {
            return;
        }
        let mut list = self.warnings.borrow_mut();
        if list.len() >= MAX_WARNINGS {
            if list.len() == MAX_WARNINGS {
                list.push((offset, "further warnings suppressed".into()));
            }
            return;
        }
        w.insert(key);
        list.push((offset, msg));
    }

    pub fn take_warnings(&self) -> Vec<(u64, String)> {
        self.warnings.borrow().clone()
    }

    fn build_revisions(&mut self) {
        let mut eofs = self.scan.eofs.clone();
        if eofs.len() > MAX_REVISIONS {
            self.warn(
                0,
                format!(
                    "{} %%EOF markers; only the first {} revisions are analysed separately",
                    eofs.len(),
                    MAX_REVISIONS
                ),
            );
            let last = eofs[eofs.len() - 1];
            eofs.truncate(MAX_REVISIONS - 1);
            eofs.push(last);
        }
        let mut revs: Vec<RevInfo> = Vec::new();
        let mut start = 0usize;
        for (eof_pos, end) in eofs {
            let startxref = self
                .scan
                .startxrefs
                .iter()
                .rev()
                .find(|(p, _)| *p >= start && *p < eof_pos)
                .and_then(|(_, v)| *v);
            revs.push(RevInfo {
                start,
                end,
                eof_pos,
                startxref,
                section: None,
                trailer: Dict::default(),
                xref_consistent: true,
                bad_entries: 0,
            });
            start = end;
        }
        // Linearized files: the first-page section ends with "startxref 0 %%EOF" (or a /Prev that
        // points forward). That marker is not an incremental update: merge it with the next one.
        let linearized = self
            .scan
            .objects
            .first()
            .and_then(|o| self.parse_at(o.start))
            .and_then(|o| o.obj.as_dict().map(|d| d.has(b"Linearized")))
            .unwrap_or(false);
        if linearized && revs.len() >= 2 && revs[0].startxref == Some(0) {
            let first = revs.remove(0);
            revs[0].start = first.start;
            self.warn(
                first.eof_pos as u64,
                "linearized file: first-page %%EOF merged into revision 1".into(),
            );
        }
        for r in revs.iter_mut() {
            if r.startxref.is_none() {
                self.warn(
                    r.eof_pos as u64,
                    "no startxref value before this %%EOF".into(),
                );
            }
        }
        // Xref sections.
        for r in revs.iter_mut() {
            let (sec, consistent) = self.read_section(r);
            r.section = sec;
            r.xref_consistent = consistent;
        }
        // Trailers with inheritance.
        let mut prev: Option<Dict> = None;
        for (k, r) in revs.iter_mut().enumerate() {
            let mut t = r
                .section
                .as_ref()
                .map(|s| s.trailer.clone())
                .unwrap_or_default();
            if r.section.as_ref().is_some_and(|s| s.kind == XrefKind::Scan) || r.section.is_none() {
                // Look for a "trailer" dictionary in this revision's bytes.
                if let Some(tp) = obj::rfind(self.data, b"trailer", r.eof_pos)
                    && tp >= r.start
                    && let Some(Obj::Dict(d)) = obj::Parser::new(self.data, tp + 7).object(0)
                {
                    t = d;
                }
            }
            if let Some(p) = &prev {
                for key in [&b"Root"[..], b"Info", b"Encrypt"] {
                    if !t.has(key)
                        && let Some(v) = p.get(key)
                    {
                        self.warnings.borrow_mut().push((
                            r.eof_pos as u64,
                            format!(
                                "revision {} trailer has no /{}; the previous one is used",
                                k + 1,
                                String::from_utf8_lossy(key)
                            ),
                        ));
                        t.0.push((key.to_vec(), v.clone()));
                    }
                }
            }
            prev = Some(t.clone());
            r.trailer = t;
        }
        // /Prev chain check.
        for k in 1..revs.len() {
            let prev_sx = revs[k - 1].section.as_ref().map(|s| s.offset as i64);
            let p = revs[k]
                .section
                .as_ref()
                .and_then(|s| s.trailer.int(b"Prev"));
            if let (Some(p), Some(e)) = (p, prev_sx)
                && p != e
            {
                let at = revs[k].eof_pos as u64;
                self.warn(
                    at,
                    format!(
                        "revision {} trailer /Prev is {} but revision {} xref is at {}",
                        k + 1,
                        p,
                        k,
                        e
                    ),
                );
            }
            if p.is_none()
                && revs[k]
                    .section
                    .as_ref()
                    .is_some_and(|s| s.kind != XrefKind::Scan)
            {
                self.warn(
                    revs[k].eof_pos as u64,
                    format!("revision {} trailer has no /Prev", k + 1),
                );
            }
        }
        self.encrypted = revs.iter().any(|r| r.trailer.has(b"Encrypt"));
        for (k, r) in revs.iter().enumerate() {
            if let Some(s) = &r.section {
                for num in s.entries.keys() {
                    self.by_num.entry(*num).or_default().push(k + 1);
                }
            }
        }
        self.revs = revs;
        // Consistency of compressed entries needs the full revision list.
        for k in 1..=self.revs.len() {
            self.check_compressed(k);
        }
    }

    fn parse_ctx_none() -> ObjCtx<'static> {
        ObjCtx {
            resolve_len: &|_| None,
            endstreams: None,
        }
    }

    fn charge(&self, n: u64) -> bool {
        let b = self.parse_budget.get();
        if b < n {
            self.parse_budget.set(0);
            self.warn(
                0,
                "parse budget exhausted; remaining objects not parsed".into(),
            );
            return false;
        }
        self.parse_budget.set(b - n);
        true
    }

    /// Parses the indirect object at a file offset (no xref lookups for /Length beyond direct
    /// values and the latest-revision map).
    pub fn parse_at(&self, off: usize) -> Option<Rc<IndirectObj>> {
        if let Some(c) = self.file_cache.borrow().get(&off) {
            return c.clone();
        }
        if self.parse_budget.get() == 0 {
            return None;
        }
        let resolve_len = |r: Ref| -> Option<i64> {
            // Indirect /Length: resolve to an integer object without recursion into streams.
            let off = self.length_offset(r)?;
            let lctx = Self::parse_ctx_none();
            parse_indirect(self.data, off, &lctx)
                .ok()
                .and_then(|o| o.obj.as_int())
        };
        let ctx = ObjCtx {
            resolve_len: &resolve_len,
            endstreams: Some(&self.scan.endstreams),
        };
        let res = match parse_indirect(self.data, off, &ctx) {
            Ok(o) => {
                self.charge((o.end - o.start) as u64);
                Some(Rc::new(o))
            }
            Err(reached) => {
                self.charge(reached.saturating_sub(off) as u64);
                None
            }
        };
        self.file_cache.borrow_mut().insert(off, res.clone());
        res
    }

    fn length_offset(&self, r: Ref) -> Option<usize> {
        // Latest in-use entry for r, else the scan.
        if let Some(revs) = self.by_num.get(&r.0) {
            for k in revs.iter().rev() {
                if let Some(XEntry::InUse { offset, .. }) = self
                    .revs
                    .get(k - 1)
                    .and_then(|rv| rv.section.as_ref())
                    .and_then(|s| s.entries.get(&r.0))
                {
                    return usize::try_from(*offset).ok();
                }
            }
        }
        self.scan_index.get(&r).and_then(|v| v.last().copied())
    }

    fn read_section(&self, r: &RevInfo) -> (Option<Section>, bool) {
        let data = self.data;
        let Some(sx) = r.startxref else {
            return (self.scan_section(r), false);
        };
        let Ok(sx) = usize::try_from(sx) else {
            return (self.scan_section(r), false);
        };
        let mut consistent = true;
        let mut at = sx;
        let valid_here = word_at(data, sx, b"xref") || read_obj_header(data, sx).is_some();
        if !valid_here || sx >= r.end {
            consistent = false;
            self.warn(
                r.eof_pos as u64,
                format!(
                    "startxref {} does not point at an xref table or xref stream inside this revision",
                    sx
                ),
            );
            // Nearest xref keyword or xref stream in this revision, used with a warning.
            let kw = self
                .scan
                .xref_kws
                .iter()
                .rev()
                .find(|&&p| p >= r.start && p < r.eof_pos)
                .copied();
            let xs = self
                .scan
                .objects
                .iter()
                .rev()
                .filter(|o| o.start >= r.start && o.start < r.eof_pos)
                .find(|o| {
                    self.parse_at(o.start)
                        .is_some_and(|io| io.obj.as_dict().is_some_and(|d| d.type_is(b"XRef")))
                })
                .map(|o| o.start);
            match kw.max(xs) {
                Some(p) => {
                    self.warn(p as u64, format!("xref section found by scanning at {}", p));
                    at = p;
                }
                None => return (self.scan_section(r), false),
            }
        }
        let sec = if word_at(data, at, b"xref") {
            self.read_table(at, r)
        } else {
            self.read_xref_stream(at, r)
        };
        let Some(mut sec) = sec else {
            self.warn(at as u64, "xref section could not be parsed".into());
            return (self.scan_section(r), false);
        };
        // Hybrid: /XRefStm.
        if sec.kind == XrefKind::Table
            && let Some(xs) = sec.trailer.int(b"XRefStm")
        {
            match usize::try_from(xs)
                .ok()
                .and_then(|xs| self.read_xref_stream(xs, r))
            {
                Some(st) => {
                    sec.kind = XrefKind::Hybrid;
                    for (num, e) in st.entries {
                        match sec.entries.get(&num) {
                            None | Some(XEntry::Free) => {
                                sec.entries.insert(num, e);
                            }
                            Some(old) if *old != e && e != XEntry::Free => {
                                self.warn(
                                    xs as u64,
                                    format!(
                                        "hybrid xref: object {} is listed differently in the table and in /XRefStm",
                                        num
                                    ),
                                );
                            }
                            _ => {}
                        }
                    }
                }
                None => {
                    consistent = false;
                    self.warn(
                        at as u64,
                        format!("/XRefStm {} is not a valid xref stream", xs),
                    );
                }
            }
        }
        // In-use entries must point at "N G obj" inside this revision.
        let mut bad = 0usize;
        for (num, e) in &sec.entries {
            if let XEntry::InUse { offset, gen_ } = *e {
                if *num == 0 {
                    continue;
                }
                let ok = usize::try_from(offset).ok().is_some_and(|o| {
                    o < r.end && {
                        let mut o2 = o;
                        let mut skipped = 0;
                        while skipped < 4 && data.get(o2).is_some_and(|&b| obj::is_ws(b)) {
                            o2 += 1;
                            skipped += 1;
                        }
                        read_obj_header(data, o2).is_some_and(|(n, g, _)| n == *num && g == gen_)
                    }
                });
                if !ok {
                    bad += 1;
                    if bad <= 20 {
                        self.warn(
                            offset,
                            format!(
                                "xref entry for object {} {} points at offset {} which does not hold that object",
                                num, gen_, offset
                            ),
                        );
                    }
                }
            }
        }
        if bad > 20 {
            self.warn(
                sec.offset as u64,
                format!("{} xref entries in total do not point at their object", bad),
            );
        }
        if bad > 0 {
            consistent = false;
        }
        (Some(sec), consistent)
    }

    fn scan_section(&self, r: &RevInfo) -> Option<Section> {
        self.warn(
            r.eof_pos as u64,
            "revision without usable xref: objects taken from a linear scan of its bytes".into(),
        );
        let mut entries = BTreeMap::new();
        for o in &self.scan.objects {
            if o.start >= r.start && o.start < r.eof_pos {
                entries.insert(
                    o.num,
                    XEntry::InUse {
                        offset: o.start as u64,
                        gen_: o.gen_,
                    },
                );
            }
        }
        Some(Section {
            offset: r
                .startxref
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(r.eof_pos),
            kind: XrefKind::Scan,
            entries,
            trailer: Dict::default(),
        })
    }

    fn read_table(&self, at: usize, r: &RevInfo) -> Option<Section> {
        let data = self.data;
        let mut lx = Lexer::new(data, at + 4);
        let mut entries = BTreeMap::new();
        loop {
            let save = lx.pos;
            match lx.next_tok() {
                Some((_, Tok::Int(start))) => {
                    let Some((_, Tok::Int(count))) = lx.next_tok() else {
                        self.warn(save as u64, "malformed xref subsection header".into());
                        return None;
                    };
                    if start < 0 || count < 0 {
                        return None;
                    }
                    let remaining = (r.end.saturating_sub(lx.pos) / 18 + 1) as i64;
                    let count = if count > remaining {
                        self.warn(save as u64, "xref subsection count exceeds the data".into());
                        remaining
                    } else {
                        count
                    };
                    for i in 0..count {
                        let num = start.saturating_add(i);
                        let (
                            Some((_, Tok::Int(off))),
                            Some((_, Tok::Int(g))),
                            Some((p, Tok::Keyword(k))),
                        ) = (lx.next_tok(), lx.next_tok(), lx.next_tok())
                        else {
                            self.warn(lx.pos as u64, "malformed xref entry".into());
                            return None;
                        };
                        let Ok(num) = u32::try_from(num) else { break };
                        let e = match k.as_slice() {
                            b"n" if off >= 0 => XEntry::InUse {
                                offset: off as u64,
                                gen_: g.clamp(0, 65535) as u16,
                            },
                            b"f" => XEntry::Free,
                            _ => {
                                self.warn(p as u64, "malformed xref entry type".into());
                                return None;
                            }
                        };
                        entries.insert(num, e);
                    }
                }
                Some((_, Tok::Keyword(k))) if k == b"trailer" => {
                    let t = obj::Parser::new(data, lx.pos).object(0)?;
                    let Obj::Dict(t) = t else { return None };
                    return Some(Section {
                        offset: at,
                        kind: XrefKind::Table,
                        entries,
                        trailer: t,
                    });
                }
                _ => {
                    self.warn(save as u64, "xref table without trailer".into());
                    return None;
                }
            }
        }
    }

    fn read_xref_stream(&self, at: usize, r: &RevInfo) -> Option<Section> {
        let io = self.parse_at(at)?;
        let st = io.obj.as_stream()?;
        let d = &st.dict;
        if !d.type_is(b"XRef") {
            self.warn(
                at as u64,
                "object at startxref is not an xref stream".into(),
            );
            return None;
        }
        if st.data_start as usize >= r.end {
            return None;
        }
        let raw = self.raw_stream(st)?;
        let data = match filters::decode(d, raw) {
            Ok((v, note)) => {
                if let Some(n) = note {
                    self.warn(at as u64, format!("xref stream: {}", n));
                }
                v
            }
            Err(e) => {
                self.warn(at as u64, format!("xref stream cannot be decoded: {:?}", e));
                return None;
            }
        };
        let w: Vec<usize> = d
            .get(b"W")?
            .as_array()?
            .iter()
            .map(|o| o.as_int().and_then(|v| usize::try_from(v).ok()))
            .collect::<Option<Vec<_>>>()?;
        if w.len() != 3 || w.iter().any(|&x| x > 8) {
            self.warn(at as u64, "xref stream /W is invalid".into());
            return None;
        }
        let size = d.int(b"Size").unwrap_or(0).max(0);
        let index: Vec<i64> = match d.get(b"Index").and_then(Obj::as_array) {
            Some(a) => a.iter().filter_map(Obj::as_int).collect(),
            None => vec![0, size],
        };
        let rowlen = w[0] + w[1] + w[2];
        if rowlen == 0 {
            return None;
        }
        let mut entries = BTreeMap::new();
        let mut rows = data.chunks_exact(rowlen);
        'outer: for pair in index.chunks(2) {
            let (Some(&start), Some(&count)) = (pair.first(), pair.get(1)) else {
                break;
            };
            if start < 0 || count < 0 {
                break;
            }
            for i in 0..count {
                let Some(row) = rows.next() else {
                    self.warn(at as u64, "xref stream shorter than its /Index".into());
                    break 'outer;
                };
                let field = |s: usize, l: usize| -> u64 {
                    row.get(s..s + l)
                        .unwrap_or(&[])
                        .iter()
                        .fold(0u64, |a, &b| (a << 8) | u64::from(b))
                };
                let t = if w[0] == 0 { 1 } else { field(0, w[0]) };
                let f2 = field(w[0], w[1]);
                let f3 = field(w[0] + w[1], w[2]);
                let Ok(num) = u32::try_from(start.saturating_add(i)) else {
                    break 'outer;
                };
                let e = match t {
                    0 => XEntry::Free,
                    1 => XEntry::InUse {
                        offset: f2,
                        gen_: f3.min(65535) as u16,
                    },
                    2 => XEntry::Compressed {
                        stm: f2.min(u64::from(u32::MAX)) as u32,
                        idx: f3.min(u64::from(u32::MAX)) as u32,
                    },
                    // Unknown types are treated as null references (spec 7.5.8.3).
                    _ => continue,
                };
                entries.insert(num, e);
            }
        }
        Some(Section {
            offset: at,
            kind: XrefKind::Stream,
            entries,
            trailer: d.clone(),
        })
    }

    fn check_compressed(&mut self, k: usize) {
        let entries: Vec<(u32, u32, u32)> =
            match self.revs.get(k - 1).and_then(|r| r.section.as_ref()) {
                Some(s) => s
                    .entries
                    .iter()
                    .filter_map(|(n, e)| match e {
                        XEntry::Compressed { stm, idx } => Some((*n, *stm, *idx)),
                        _ => None,
                    })
                    .collect(),
                None => return,
            };
        let mut bad = 0;
        for (num, stm, idx) in entries {
            let ok = self
                .objstm(k, stm)
                .is_some_and(|os| os.objects.get(idx as usize).is_some_and(|(n, _)| *n == num));
            if !ok {
                bad += 1;
                if bad <= 20 {
                    self.warn(
                        0,
                        format!(
                            "xref entry for object {} points into object stream {} at index {} which does not hold it",
                            num, stm, idx
                        ),
                    );
                }
            }
        }
        if bad > 0
            && !self.encrypted
            && let Some(r) = self.revs.get_mut(k - 1)
        {
            r.xref_consistent = false;
        }
    }

    pub fn raw_stream(&self, st: &obj::Stream) -> Option<&'a [u8]> {
        let s = usize::try_from(st.data_start).ok()?;
        let l = usize::try_from(st.data_len).ok()?;
        self.data.get(s..s.checked_add(l)?)
    }

    /// Decoded data of a file-level stream; None (+ warning) when it cannot be decoded.
    pub fn decode_stream(&self, st: &obj::Stream, what: &str) -> Option<Vec<u8>> {
        if self.encrypted && !st.dict.type_is(b"XRef") {
            return None;
        }
        let raw = self.raw_stream(st)?;
        let left = self.decode_budget.get();
        if left == 0 {
            return None;
        }
        match filters::decode(&st.dict, raw) {
            Ok((v, note)) => {
                if let Some(n) = note {
                    self.warn(st.data_start, format!("{}: {}", what, n));
                }
                self.decode_budget.set(left.saturating_sub(v.len() as u64));
                if self.decode_budget.get() == 0 {
                    self.warn(
                        0,
                        "decode budget exhausted; further streams are not decoded".into(),
                    );
                }
                Some(v)
            }
            Err(DecodeError::Unsupported(f)) => {
                self.warn(st.data_start, format!("{}: filter {} not decoded", what, f));
                None
            }
            Err(DecodeError::Corrupt(m)) => {
                self.warn(st.data_start, format!("{}: {}", what, m));
                None
            }
        }
    }

    /// Latest entry for `num` visible in revision `rev` (1-based).
    pub fn entry(&self, rev: usize, num: u32) -> Option<(usize, XEntry)> {
        let v = self.by_num.get(&num)?;
        let i = v.partition_point(|&k| k <= rev);
        let k = *v.get(i.checked_sub(1)?)?;
        let e = *self.revs.get(k - 1)?.section.as_ref()?.entries.get(&num)?;
        Some((k, e))
    }

    fn objstm(&self, rev: usize, stm: u32) -> Option<Rc<ObjStm>> {
        let (_, e) = self.entry(rev, stm)?;
        let XEntry::InUse { offset, .. } = e else {
            return None;
        };
        let off = usize::try_from(offset).ok()?;
        if let Some(c) = self.objstm_cache.borrow().get(&off) {
            return c.clone();
        }
        let res = self.load_objstm(off).map(Rc::new);
        self.objstm_cache.borrow_mut().insert(off, res.clone());
        res
    }

    fn load_objstm(&self, off: usize) -> Option<ObjStm> {
        let io = self.parse_at(off)?;
        let st = io.obj.as_stream()?;
        if !st.dict.type_is(b"ObjStm") {
            self.warn(
                off as u64,
                "object stream reference to a non-/ObjStm object".into(),
            );
            return None;
        }
        let n = st.dict.int(b"N")?.clamp(0, 1_000_000) as usize;
        let first = usize::try_from(st.dict.int(b"First")?).ok()?;
        let data = self.decode_stream(st, "object stream")?;
        let mut lx = Lexer::new(&data, 0);
        let mut hdr = Vec::new();
        for _ in 0..n {
            match (lx.next_tok(), lx.next_tok()) {
                (Some((_, Tok::Int(num))), Some((_, Tok::Int(o)))) if num >= 0 && o >= 0 => {
                    hdr.push((num.min(i64::from(u32::MAX)) as u32, o as usize))
                }
                _ => {
                    self.warn(off as u64, "object stream header is malformed".into());
                    break;
                }
            }
        }
        if lx.pos > first {
            self.warn(
                off as u64,
                "object stream /First overlaps its header".into(),
            );
        }
        let mut objects = Vec::with_capacity(hdr.len());
        for (num, o) in hdr {
            let pos = first.saturating_add(o);
            let parsed = if pos < data.len() {
                if !self.charge(64) {
                    None
                } else {
                    obj::Parser::new(&data, pos).object(0)
                }
            } else {
                None
            };
            objects.push((num, parsed));
        }
        Some(ObjStm { objects })
    }

    /// Resolves `r` as seen by revision `rev`. Returns the object and its location.
    pub fn resolve_loc(&self, rev: usize, r: Ref) -> Option<(Obj, Loc)> {
        let rev_end = self.revs.get(rev.checked_sub(1)?)?.end;
        match self.entry(rev, r.0) {
            Some((_, XEntry::InUse { offset, gen_ })) => {
                if gen_ != r.1 {
                    return None;
                }
                let off = usize::try_from(offset).ok()?;
                let mut o2 = off;
                let mut skipped = 0;
                while skipped < 4 && self.data.get(o2).is_some_and(|&b| obj::is_ws(b)) {
                    o2 += 1;
                    skipped += 1;
                }
                if let Some(io) = self.parse_at(o2)
                    && io.num == r.0
                    && io.gen_ == r.1
                {
                    return Some((io.obj.clone(), Loc::File(o2)));
                }
                self.scan_fallback(r, rev_end, offset)
            }
            Some((_, XEntry::Compressed { stm, idx })) => {
                let os = self.objstm(rev, stm)?;
                let found = match os.objects.get(idx as usize) {
                    Some((n, o)) if *n == r.0 => o.clone(),
                    _ => os
                        .objects
                        .iter()
                        .find(|(n, _)| *n == r.0)
                        .and_then(|(_, o)| o.clone()),
                };
                let offset = self.entry(rev, stm).and_then(|(_, e)| match e {
                    XEntry::InUse { offset, .. } => usize::try_from(offset).ok(),
                    _ => None,
                })?;
                found.map(|o| (o, Loc::InStream(offset, stm)))
            }
            Some((_, XEntry::Free)) => None,
            None => {
                let k = rev.checked_sub(1)?;
                if self
                    .revs
                    .iter()
                    .take(k + 1)
                    .all(|rv| rv.section.as_ref().is_none_or(|s| s.kind == XrefKind::Scan))
                {
                    return self.scan_fallback(r, rev_end, 0);
                }
                None
            }
        }
    }

    fn scan_fallback(&self, r: Ref, rev_end: usize, claimed: u64) -> Option<(Obj, Loc)> {
        let offs = self.scan_index.get(&r)?;
        let off = *offs.iter().rev().find(|&&o| o < rev_end)?;
        let io = self.parse_at(off)?;
        if claimed != 0 {
            self.warn(
                claimed,
                format!(
                    "object {} {} resolved at offset {} found by scanning, not at the xref offset {}",
                    r.0, r.1, off, claimed
                ),
            );
        }
        Some((io.obj.clone(), Loc::File(off)))
    }

    pub fn resolve(&self, rev: usize, r: Ref) -> Option<Obj> {
        self.resolve_loc(rev, r).map(|(o, _)| o)
    }

    /// Follows references (bounded) until a direct object.
    pub fn deref(&self, rev: usize, o: &Obj) -> Option<Obj> {
        let mut cur = o.clone();
        for _ in 0..16 {
            match cur {
                Obj::Ref(n, g) => cur = self.resolve(rev, (n, g))?,
                other => return Some(other),
            }
        }
        None
    }

    pub fn get_dict(&self, rev: usize, o: &Obj) -> Option<Dict> {
        match self.deref(rev, o)? {
            Obj::Dict(d) => Some(d),
            Obj::Stream(s) => Some(s.dict),
            _ => None,
        }
    }

    pub fn trailer(&self, rev: usize) -> Option<&Dict> {
        self.revs.get(rev.checked_sub(1)?).map(|r| &r.trailer)
    }

    pub fn catalog(&self, rev: usize) -> Option<Dict> {
        let root = self.trailer(rev)?.get(b"Root")?.clone();
        self.get_dict(rev, &root)
    }

    /// Object numbers listed in revision `rev`'s own xref section.
    pub fn section_entries(&self, rev: usize) -> Vec<(u32, XEntry)> {
        self.revs
            .get(rev.wrapping_sub(1))
            .and_then(|r| r.section.as_ref())
            .map(|s| s.entries.iter().map(|(k, v)| (*k, *v)).collect())
            .unwrap_or_default()
    }

    /// Generation of the in-use entry for `num` in revision `rev` (0 for compressed objects).
    pub fn gen_of(&self, rev: usize, num: u32) -> Option<u16> {
        match self.entry(rev, num)?.1 {
            XEntry::InUse { gen_, .. } => Some(gen_),
            XEntry::Compressed { .. } => Some(0),
            XEntry::Free => None,
        }
    }

    pub fn is_live(&self, rev: usize, num: u32) -> bool {
        rev > 0 && self.gen_of(rev, num).is_some()
    }
}
