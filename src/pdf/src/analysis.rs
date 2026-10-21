//! Builds a `PdfReport` from a parsed `Doc`: revisions and their deltas, metadata, fonts,
//! content signals, signatures, fingerprints and the findings derived from them.

use crate::doc::{Doc, Loc, XEntry, XrefKind};
use crate::fingerprint::{self, RevTraits, Strings};
use crate::meta::{self, parse_pdf_instant, parse_xmp_instant, same_instant};
use crate::model::*;
use crate::obj::{Dict, Obj, Ref, id_str, text_string};
use crate::pages::{self, Hashing};
use crate::sig;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const IMAGE_PAGE_COVER: f64 = 0.5;
const MIN_REVISION_GAP_SECS: i64 = 3600;
const MAX_PAGE_FINDINGS: usize = 100;
const CONTENT_OP_BUDGET: u64 = 50_000_000;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn join<T: ToString>(v: &[T]) -> String {
    v.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Same object content (streams: same dictionary and same raw bytes).
fn same_content(doc: &Doc, a: &Obj, b: &Obj) -> bool {
    match (a, b) {
        (Obj::Stream(x), Obj::Stream(y)) => {
            let dx: Vec<_> = x.dict.0.iter().filter(|(k, _)| k != b"Length").collect();
            let dy: Vec<_> = y.dict.0.iter().filter(|(k, _)| k != b"Length").collect();
            dx == dy && doc.raw_stream(x) == doc.raw_stream(y)
        }
        _ => a == b,
    }
}

/// Keys whose values differ between `old` (as seen by revision `rev - 1`) and `new` (revision
/// `rev`). Values are compared deeply, so rewriting an object in direct instead of indirect form
/// is not a change; structural links (/Parent, /P, /Kids) are compared as references.
fn diff_keys(doc: &Doc, rev: usize, old: &Dict, new: &Dict) -> BTreeSet<Vec<u8>> {
    let mut ho = Hashing::new(doc, rev.saturating_sub(1));
    let mut hn = Hashing::new(doc, rev);
    let mut out = BTreeSet::new();
    for k in old.keys().chain(new.keys()) {
        let (a, b) = (old.get(k), new.get(k));
        let same = if matches!(k, b"Parent" | b"P" | b"Kids") {
            a == b
        } else {
            match (a, b) {
                (Some(x), Some(y)) => x == y || ho.hash(x, 0) == hn.hash(y, 0),
                (None, None) => true,
                _ => false,
            }
        };
        if !same {
            out.insert(k.to_vec());
        }
    }
    out
}

fn subset(keys: &BTreeSet<Vec<u8>>, allowed: &[&[u8]]) -> bool {
    keys.iter().all(|k| allowed.contains(&k.as_slice()))
}

const ANNOT_SUBTYPES: &[&[u8]] = &[
    b"Text",
    b"Link",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Stamp",
    b"Caret",
    b"Ink",
    b"Popup",
    b"FileAttachment",
    b"Sound",
    b"Movie",
    b"Widget",
    b"Screen",
    b"PrinterMark",
    b"TrapNet",
    b"Watermark",
    b"3D",
    b"Redact",
    b"RichMedia",
    b"Projection",
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Signature,
    FormFill,
    Annotations,
    Metadata,
    Other,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Signature => "signature",
            Kind::FormFill => "form_fill",
            Kind::Annotations => "annotations",
            Kind::Metadata => "metadata",
            Kind::Other => "other",
        }
    }
}

enum Class {
    Kind(Kind),
    Neutral,
    Deferred,
}

/// Collects references reachable from `o` (bounded).
fn reach(doc: &Doc, rev: usize, o: &Obj, out: &mut HashSet<Ref>, depth: usize) {
    if depth > 6 || out.len() > 10_000 {
        return;
    }
    match o {
        Obj::Ref(n, g) => {
            if out.insert((*n, *g))
                && let Some(t) = doc.resolve(rev, (*n, *g))
            {
                reach(doc, rev, &t, out, depth + 1);
            }
        }
        Obj::Array(a) => a.iter().for_each(|x| reach(doc, rev, x, out, depth + 1)),
        Obj::Dict(d) => {
            d.0.iter()
                .filter(|(k, _)| k != b"Parent" && k != b"P")
                .for_each(|(_, v)| reach(doc, rev, v, out, depth + 1))
        }
        Obj::Stream(s) => s
            .dict
            .0
            .iter()
            .for_each(|(_, v)| reach(doc, rev, v, out, depth + 1)),
        _ => {}
    }
}

fn field_type(doc: &Doc, rev: usize, d: &Dict) -> Option<Vec<u8>> {
    let mut cur = d.clone();
    for _ in 0..16 {
        if let Some(ft) = cur.name(b"FT") {
            return Some(ft.to_vec());
        }
        let p = cur.get(b"Parent")?;
        cur = doc.get_dict(rev, p)?;
    }
    None
}

struct RevCtx {
    info_ref: Option<Ref>,
    metadata_ref: Option<Ref>,
    catalog_ref: Option<Ref>,
    acroform_ref: Option<Ref>,
    containers: HashSet<Ref>,
    neutral_reach: HashSet<Ref>,
    dss_reach: HashSet<Ref>,
}

fn rev_ctx(doc: &Doc, rev: usize, pages: &[pages::Page]) -> RevCtx {
    let t = doc.trailer(rev).cloned().unwrap_or_default();
    let catalog_ref = t.get(b"Root").and_then(Obj::as_ref);
    let cat = doc.catalog(rev).unwrap_or_default();
    let acro = cat.get(b"AcroForm");
    let mut containers = HashSet::new();
    for p in pages {
        if let Some(r) = p.dict.get(b"Annots").and_then(Obj::as_ref) {
            containers.insert(r);
        }
    }
    let mut neutral_reach = HashSet::new();
    if let Some(af) = acro.and_then(|a| doc.get_dict(rev, a)) {
        if let Some(r) = af.get(b"Fields").and_then(Obj::as_ref) {
            containers.insert(r);
        }
        if let Some(dr) = af.get(b"DR") {
            reach(doc, rev, dr, &mut neutral_reach, 0);
        }
    }
    let mut dss_reach = HashSet::new();
    if let Some(dss) = cat.get(b"DSS") {
        reach(doc, rev, dss, &mut dss_reach, 0);
    }
    RevCtx {
        info_ref: t.get(b"Info").and_then(Obj::as_ref),
        metadata_ref: cat.get(b"Metadata").and_then(Obj::as_ref),
        catalog_ref,
        acroform_ref: acro.and_then(Obj::as_ref),
        containers,
        neutral_reach,
        dss_reach,
    }
}

/// Classifies one added/changed object of revision `rev`. `ap_owner` receives the objects
/// reachable from annotation appearances together with the annotation's kind.
fn classify_obj(
    doc: &Doc,
    rev: usize,
    r: Ref,
    new: &Obj,
    old: Option<&Obj>,
    ctx: &RevCtx,
    ap_owner: &mut HashMap<Ref, Kind>,
) -> Class {
    if Some(r) == ctx.info_ref || Some(r) == ctx.metadata_ref {
        return Class::Kind(Kind::Metadata);
    }
    if ctx.containers.contains(&r) {
        return Class::Neutral;
    }
    let Some(d) = new.as_dict() else {
        return Class::Deferred;
    };
    if d.type_is(b"XRef") || d.type_is(b"ObjStm") {
        return Class::Neutral;
    }
    if d.type_is(b"Metadata") {
        return Class::Kind(Kind::Metadata);
    }
    if d.get(b"ByteRange").is_some() && d.get(b"Contents").is_some() {
        return Class::Kind(Kind::Signature);
    }
    let old_d = old.and_then(Obj::as_dict);
    if Some(r) == ctx.catalog_ref || d.type_is(b"Catalog") {
        let prev = old_d.cloned().or_else(|| doc.catalog(rev - 1));
        return match prev {
            Some(p) => {
                let mut keys = diff_keys(doc, rev, &p, d);
                // A direct /AcroForm dictionary: only fields/signature flags may change.
                if keys.contains(b"AcroForm".as_slice())
                    && let (Some(Obj::Dict(a)), Some(Obj::Dict(b))) =
                        (p.get(b"AcroForm"), d.get(b"AcroForm"))
                    && subset(
                        &diff_keys(doc, rev, a, b),
                        &[b"Fields", b"SigFlags", b"DR", b"DA", b"NeedAppearances"],
                    )
                {
                    keys.remove(b"AcroForm".as_slice());
                }
                if subset(
                    &keys,
                    &[b"AcroForm", b"Perms", b"DSS", b"Metadata", b"Extensions"],
                ) {
                    Class::Neutral
                } else {
                    Class::Kind(Kind::Other)
                }
            }
            None => Class::Kind(Kind::Other),
        };
    }
    if Some(r) == ctx.acroform_ref {
        return match old_d {
            Some(p)
                if subset(
                    &diff_keys(doc, rev, p, d),
                    &[b"Fields", b"SigFlags", b"DR", b"DA", b"NeedAppearances"],
                ) =>
            {
                Class::Neutral
            }
            None => Class::Neutral,
            _ => Class::Kind(Kind::Other),
        };
    }
    if d.type_is(b"Page") {
        let Some(p) = old_d else {
            return Class::Kind(Kind::Other);
        };
        let keys = diff_keys(doc, rev, p, d);
        if !subset(&keys, &[b"Annots"]) {
            return Class::Kind(Kind::Other);
        }
        let set = |x: &Dict| -> HashSet<Ref> {
            x.get(b"Annots")
                .and_then(|a| doc.deref(rev, a))
                .and_then(|a| {
                    a.as_array()
                        .map(|v| v.iter().filter_map(Obj::as_ref).collect())
                })
                .unwrap_or_default()
        };
        let removed = set(p).difference(&set(d)).count();
        return if removed > 0 {
            Class::Kind(Kind::Annotations)
        } else {
            Class::Neutral
        };
    }
    let subtype = d.name(b"Subtype");
    let is_annot = d.type_is(b"Annot")
        || (d.has(b"Rect") && subtype.is_some_and(|s| ANNOT_SUBTYPES.contains(&s)));
    let is_field = d.has(b"FT") || (d.has(b"T") && (d.has(b"Kids") || d.has(b"Parent")));
    if is_annot || is_field {
        let widget = subtype == Some(b"Widget") || is_field;
        let kind = if widget {
            if field_type(doc, rev, d).as_deref() == Some(b"Sig") {
                Kind::Signature
            } else {
                match old_d {
                    Some(p)
                        if subset(
                            &diff_keys(doc, rev, p, d),
                            &[b"V", b"AS", b"AP", b"M", b"MK"],
                        ) =>
                    {
                        Kind::FormFill
                    }
                    _ => Kind::Other,
                }
            }
        } else {
            Kind::Annotations
        };
        let mut reached = HashSet::new();
        for key in [&b"AP"[..], b"MK", b"Popup", b"V"] {
            if let Some(v) = d.get(key) {
                reach(doc, rev, v, &mut reached, 0);
            }
        }
        for x in reached {
            ap_owner.entry(x).or_insert(kind);
        }
        return Class::Kind(kind);
    }
    if ctx.dss_reach.contains(&r) {
        return Class::Kind(Kind::Signature);
    }
    if ctx.neutral_reach.contains(&r) {
        return Class::Neutral;
    }
    Class::Deferred
}

struct Delta {
    added: Vec<Ref>,
    changed: Vec<(Ref, u16)>,
    deleted: Vec<Ref>,
}

fn delta(doc: &Doc, rev: usize) -> Delta {
    let mut d = Delta {
        added: Vec::new(),
        changed: Vec::new(),
        deleted: Vec::new(),
    };
    for (num, e) in doc.section_entries(rev) {
        if num == 0 {
            continue;
        }
        let prev_gen = if rev > 1 {
            doc.gen_of(rev - 1, num)
        } else {
            None
        };
        match e {
            XEntry::Free => {
                if let Some(g) = prev_gen {
                    d.deleted.push((num, g));
                }
            }
            _ => {
                let Some(g) = doc.gen_of(rev, num) else {
                    continue;
                };
                match prev_gen {
                    None => d.added.push((num, g)),
                    Some(pg) => {
                        let old = doc.resolve(rev - 1, (num, pg));
                        let new = doc.resolve(rev, (num, g));
                        let same = match (&old, &new) {
                            (Some(a), Some(b)) => pg == g && same_content(doc, a, b),
                            (None, None) => true,
                            _ => false,
                        };
                        if !same {
                            d.changed.push(((num, g), pg));
                        }
                    }
                }
            }
        }
    }
    d
}

fn change_kinds(doc: &Doc, rev: usize, d: &Delta, pages: &[pages::Page]) -> Vec<String> {
    let ctx = rev_ctx(doc, rev, pages);
    let mut kinds: BTreeSet<Kind> = BTreeSet::new();
    let mut ap_owner: HashMap<Ref, Kind> = HashMap::new();
    let mut deferred = Vec::new();
    let items: Vec<(Ref, Option<Ref>)> = d
        .added
        .iter()
        .map(|r| (*r, None))
        .chain(d.changed.iter().map(|(r, pg)| (*r, Some((r.0, *pg)))))
        .collect();
    for (r, old_ref) in items {
        let Some(new) = doc.resolve(rev, r) else {
            continue;
        };
        let old = old_ref.and_then(|o| doc.resolve(rev - 1, o));
        match classify_obj(doc, rev, r, &new, old.as_ref(), &ctx, &mut ap_owner) {
            Class::Kind(k) => {
                kinds.insert(k);
            }
            Class::Neutral => {}
            Class::Deferred => deferred.push(r),
        }
    }
    for r in deferred {
        kinds.insert(ap_owner.get(&r).copied().unwrap_or(Kind::Other));
    }
    if !d.deleted.is_empty() {
        kinds.insert(Kind::Other);
    }
    kinds.into_iter().map(|k| k.as_str().to_string()).collect()
}

fn is_font_dict(d: &Dict) -> bool {
    d.type_is(b"Font")
        || (d.has(b"BaseFont")
            && d.name(b"Subtype").is_some_and(|s| {
                matches!(
                    s,
                    b"Type0"
                        | b"Type1"
                        | b"MMType1"
                        | b"Type3"
                        | b"TrueType"
                        | b"CIDFontType0"
                        | b"CIDFontType2"
                )
            }))
}

fn font_embedded(doc: &Doc, rev: usize, d: &Dict) -> bool {
    if d.name(b"Subtype") == Some(b"Type3") {
        return true;
    }
    let desc_has_file =
        |fd: &Dict| fd.has(b"FontFile") || fd.has(b"FontFile2") || fd.has(b"FontFile3");
    if let Some(fd) = d.get(b"FontDescriptor").and_then(|x| doc.get_dict(rev, x))
        && desc_has_file(&fd)
    {
        return true;
    }
    if let Some(Obj::Array(desc)) = d.get(b"DescendantFonts").and_then(|x| doc.deref(rev, x))
        && let Some(df) = desc.first().and_then(|x| doc.get_dict(rev, x))
        && let Some(fd) = df.get(b"FontDescriptor").and_then(|x| doc.get_dict(rev, x))
    {
        return desc_has_file(&fd);
    }
    false
}

fn split_subset(name: &str) -> (Option<String>, String) {
    let b = name.as_bytes();
    if b.len() > 7 && b[6] == b'+' && b[..6].iter().all(u8::is_ascii_uppercase) {
        (Some(name[..6].to_string()), name[7..].to_string())
    } else {
        (None, name.to_string())
    }
}

fn base_font_name(d: &Dict) -> String {
    match d.get(b"BaseFont") {
        Some(Obj::Name(n)) => String::from_utf8_lossy(n).into_owned(),
        Some(Obj::Str(s)) => text_string(s),
        _ => match d.get(b"Name") {
            Some(Obj::Name(n)) => String::from_utf8_lossy(n).into_owned(),
            _ => "(unnamed)".into(),
        },
    }
}

/// Fonts used by page content in revision `rev`: references and base names (subset prefix removed).
fn page_fonts(doc: &Doc, rev: usize) -> (HashSet<Ref>, HashSet<String>) {
    let mut refs = HashSet::new();
    let mut names = HashSet::new();
    let mut seen_forms: HashSet<Ref> = HashSet::new();
    let mut stack: Vec<(Dict, usize)> = pages::pages(doc, rev)
        .into_iter()
        .filter_map(|p| p.resources.and_then(|r| r.as_dict().cloned()))
        .map(|d| (d, 0))
        .collect();
    let mut steps = 0usize;
    while let Some((res, depth)) = stack.pop() {
        steps += 1;
        if steps > 200_000 {
            break;
        }
        if let Some(fonts) = res.get(b"Font").and_then(|f| doc.get_dict(rev, f)) {
            for (_, v) in &fonts.0 {
                if let Some(r) = v.as_ref() {
                    refs.insert(r);
                }
                if let Some(fd) = doc.get_dict(rev, v) {
                    names.insert(split_subset(&base_font_name(&fd)).1);
                }
            }
        }
        if depth < 3
            && let Some(xo) = res.get(b"XObject").and_then(|f| doc.get_dict(rev, f))
        {
            for (_, v) in &xo.0 {
                if let Some(r) = v.as_ref()
                    && !seen_forms.insert(r)
                {
                    continue;
                }
                if let Some(Obj::Stream(s)) = doc.deref(rev, v)
                    && s.dict.name(b"Subtype") == Some(b"Form")
                    && let Some(fr) = s.dict.get(b"Resources").and_then(|x| doc.get_dict(rev, x))
                {
                    stack.push((fr, depth + 1));
                }
            }
        }
    }
    (refs, names)
}

#[derive(Default)]
struct Signals {
    js: BTreeSet<String>,
    launch: BTreeSet<String>,
    filespecs: Vec<(usize, Dict)>,
    embedded_streams: Vec<(usize, Ref, crate::obj::Stream)>,
    images: Vec<(usize, Ref, crate::obj::Stream)>,
}

fn walk_signals(o: &Obj, id: &str, rev: usize, r: Ref, s: &mut Signals, depth: usize) {
    if depth > 16 {
        return;
    }
    match o {
        Obj::Array(a) => a
            .iter()
            .take(100_000)
            .for_each(|x| walk_signals(x, id, rev, r, s, depth + 1)),
        Obj::Dict(d) => {
            let st = d.name(b"S");
            if st == Some(b"JavaScript")
                || d.has(b"JS")
                || (depth == 0 && d.has(b"JavaScript") && !d.has(b"Type"))
            {
                s.js.insert(id.to_string());
            }
            if st == Some(b"Launch") {
                s.launch.insert(id.to_string());
            }
            if d.has(b"EF") && (d.type_is(b"Filespec") || d.has(b"F") || d.has(b"UF")) {
                s.filespecs.push((rev, d.clone()));
            }
            for (_, v) in &d.0 {
                walk_signals(v, id, rev, r, s, depth + 1);
            }
        }
        Obj::Stream(st) => {
            if st.dict.type_is(b"EmbeddedFile") {
                s.embedded_streams.push((rev, r, (**st).clone()));
            }
            if st.dict.name(b"Subtype") == Some(b"Image") {
                s.images.push((rev, r, (**st).clone()));
            }
            if st.dict.has(b"JS") || st.dict.name(b"S") == Some(b"JavaScript") {
                s.js.insert(id.to_string());
            }
        }
        _ => {}
    }
}

fn collect_signals(doc: &Doc) -> Signals {
    let mut s = Signals::default();
    let mut seen: HashSet<Loc> = HashSet::new();
    for rev in 1..=doc.revs.len() {
        for (num, e) in doc.section_entries(rev) {
            if num == 0 || e == XEntry::Free {
                continue;
            }
            let Some(g) = doc.gen_of(rev, num) else {
                continue;
            };
            let Some((o, loc)) = doc.resolve_loc(rev, (num, g)) else {
                continue;
            };
            if !seen.insert(loc) {
                continue;
            }
            walk_signals(&o, &id_str((num, g)), rev, (num, g), &mut s, 0);
        }
    }
    // JavaScript name tree in the catalog.
    for rev in 1..=doc.revs.len() {
        if let Some(cat) = doc.catalog(rev)
            && let Some(names) = cat.get(b"Names").and_then(|n| doc.get_dict(rev, n))
            && names.has(b"JavaScript")
        {
            s.js.insert("catalog /Names /JavaScript".into());
        }
    }
    s
}

fn fmt_gap(secs: i64) -> String {
    let d = secs / 86_400;
    let h = secs % 86_400 / 3600;
    let m = secs % 3600 / 60;
    if d > 0 {
        format!("{} d {} h {} min", d, h, m)
    } else {
        format!("{} h {} min", h, m)
    }
}

fn header_traits(bytes: &[u8], header: usize) -> BTreeMap<String, String> {
    let mut t = BTreeMap::new();
    let rest = bytes.get(header..).unwrap_or(&[]);
    let line_end = |s: &[u8]| {
        s.iter()
            .position(|&c| c == b'\n' || c == b'\r')
            .unwrap_or(s.len())
    };
    let l1 = line_end(rest);
    if let Some(v) = rest.get(5..l1) {
        t.insert(
            "header_version".into(),
            String::from_utf8_lossy(v).trim().to_string(),
        );
    }
    // The comment lines that follow the header (binary marker, generator comments).
    let mut pos = l1;
    let mut comments = Vec::new();
    for _ in 0..4 {
        while rest.get(pos).is_some_and(|c| *c == b'\n' || *c == b'\r') {
            pos += 1;
        }
        if rest.get(pos) != Some(&b'%') {
            break;
        }
        let e = pos + line_end(&rest[pos..]);
        comments.push(&rest[pos + 1..e]);
        pos = e;
    }
    if let Some(first) = comments.first() {
        t.insert(
            "binary_comment_hex".into(),
            hex(&first[..first.len().min(8)]),
        );
    }
    let text: Vec<String> = comments
        .iter()
        .map(|c| {
            let s: String = c
                .iter()
                .filter(|b| (0x20..0x7F).contains(*b))
                .map(|&b| b as char)
                .collect();
            s.trim().to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();
    t.insert("header_comment_contains".into(), text.join(" | "));
    t
}

pub fn build(bytes: &[u8], opts: &PdfOptions) -> PdfReport {
    let mut rep = PdfReport::default();
    let header = crate::obj::find(&bytes[..bytes.len().min(1024)], b"%PDF-", 0);
    let Some(header) = header else {
        rep.findings
            .push(PdfFinding::new("NOT_A_PDF", Severity::Info));
        return rep;
    };
    rep.is_pdf = true;
    let ver: String = bytes
        .get(header + 5..)
        .unwrap_or(&[])
        .iter()
        .take(8)
        .take_while(|c| c.is_ascii_digit() || **c == b'.')
        .map(|&c| c as char)
        .collect();
    rep.version = (!ver.is_empty()).then_some(ver);
    let doc = Doc::open(bytes);
    rep.headers = doc.scan.headers.iter().map(|&h| h as u64).collect();
    rep.encrypted = doc.encrypted;
    let n = doc.revs.len();
    if n == 0 {
        doc.warn(0, "no %%EOF marker: no complete revision".into());
    }

    // ---------------------------------------------------------- revisions
    let mut all_pages: Vec<Vec<pages::Page>> = Vec::with_capacity(n);
    let mut page_sigs: Vec<Vec<u64>> = Vec::with_capacity(n);
    for k in 1..=n {
        let pg = pages::pages(&doc, k);
        if n > 1 {
            let mut hs = Hashing::new(&doc, k);
            page_sigs.push(
                pg.iter()
                    .map(|p| pages::page_signature(&mut hs, p))
                    .collect(),
            );
        } else {
            page_sigs.push(Vec::new());
        }
        all_pages.push(pg);
    }
    for k in 1..=n {
        let ri = &doc.revs[k - 1];
        let mut r = Revision {
            index: k,
            start_offset: ri.start as u64,
            end_offset: ri.end as u64,
            startxref: ri.startxref,
            xref_kind: ri
                .section
                .as_ref()
                .map_or(XrefKind::Scan, |s| s.kind)
                .as_str()
                .to_string(),
            xref_consistent: ri.xref_consistent,
            info: meta::info(&doc, k),
            xmp: meta::xmp(&doc, k),
            page_count: doc.catalog(k).map(|_| all_pages[k - 1].len() as u32),
            ..Revision::default()
        };
        if let Some(Obj::Array(ids)) = ri.trailer.get(b"ID") {
            r.id = ids.iter().filter_map(Obj::as_str).map(hex).collect();
        }
        let d = delta(&doc, k);
        if k == 1 {
            r.objects_added = doc
                .section_entries(1)
                .into_iter()
                .filter(|(num, e)| *num != 0 && *e != XEntry::Free)
                .filter_map(|(num, _)| doc.gen_of(1, num).map(|g| id_str((num, g))))
                .collect();
        } else {
            r.objects_added = d.added.iter().map(|x| id_str(*x)).collect();
            r.objects_changed = d.changed.iter().map(|(x, _)| id_str(*x)).collect();
            r.objects_deleted = d.deleted.iter().map(|x| id_str(*x)).collect();
            let prev = &page_sigs[k - 2];
            let cur = &page_sigs[k - 1];
            r.pages_changed = cur
                .iter()
                .enumerate()
                .filter(|(i, s)| prev.get(*i) != Some(s))
                .map(|(i, _)| i as u32 + 1)
                .collect();
            r.change_kind = change_kinds(&doc, k, &d, &all_pages[k - 1]);
        }
        rep.revisions.push(r);
    }
    rep.trailing_bytes = doc
        .revs
        .last()
        .map_or(0, |r| bytes.len().saturating_sub(r.end) as u64);
    rep.page_count = (n > 0 && doc.catalog(n).is_some()).then(|| all_pages[n - 1].len() as u32);

    // ---------------------------------------------------------- fonts
    let mut font_keys: HashSet<(Ref, String)> = HashSet::new();
    for k in 1..=n {
        for (num, e) in doc.section_entries(k) {
            if num == 0 || e == XEntry::Free {
                continue;
            }
            let Some(g) = doc.gen_of(k, num) else {
                continue;
            };
            let Some(Obj::Dict(d)) = doc.resolve(k, (num, g)) else {
                continue;
            };
            if !is_font_dict(&d) {
                continue;
            }
            let name = base_font_name(&d);
            if !font_keys.insert(((num, g), name.clone())) {
                continue;
            }
            let (prefix, _) = split_subset(&name);
            rep.fonts.push(FontUse {
                embedded: font_embedded(&doc, k, &d),
                name,
                subset_prefix: prefix,
                first_revision: k,
                object: id_str((num, g)),
            });
        }
    }

    // ---------------------------------------------------------- signatures
    let sigs = sig::find_signatures(&doc);
    for s in &sigs {
        let field = s
            .field
            .clone()
            .unwrap_or_else(|| format!("(unnamed {})", id_str(s.obj)));
        let br: Vec<u64> = s
            .dict
            .get(b"ByteRange")
            .and_then(Obj::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|o| o.as_int().and_then(|v| u64::try_from(v).ok()))
            .collect();
        let mut out = Signature {
            field: field.clone(),
            sub_filter: s
                .dict
                .name(b"SubFilter")
                .map(|x| String::from_utf8_lossy(x).into_owned()),
            byte_range: br.clone(),
            docmdp: sig::docmdp_level(&doc, s),
            ..Signature::default()
        };
        let span = match s.loc {
            Loc::File(off) => sig::contents_span(bytes, off),
            Loc::InStream(..) => None,
        };
        let ranges_ok = br.len() == 4 && br[0] == 0 && br[0] + br[1] <= br[2];
        let end = if ranges_ok {
            br[2].checked_add(br[3])
        } else {
            None
        };
        if let Some(end) = end {
            out.revision = doc
                .revs
                .iter()
                .position(|r| r.end as u64 == end || (r.eof_pos + 5) as u64 == end)
                .map(|i| i + 1);
        }
        out.covers_revision = ranges_ok
            && out.revision.is_some()
            && span.is_some_and(|(a, b)| a as u64 == br[1] && b as u64 == br[2]);
        let parts: Option<Vec<&[u8]>> = if ranges_ok && end.is_some_and(|e| e <= bytes.len() as u64)
        {
            Some(vec![
                &bytes[br[0] as usize..(br[0] + br[1]) as usize],
                &bytes[br[2] as usize..(br[2] + br[3]) as usize],
            ])
        } else {
            None
        };
        let supported = matches!(
            out.sub_filter.as_deref(),
            Some("adbe.pkcs7.detached") | Some("ETSI.CAdES.detached")
        );
        if let (true, Some(parts), Some(Obj::Str(cms))) =
            (supported, &parts, s.dict.get(b"Contents"))
        {
            let c = sig::check_cms(cms, parts);
            out.digest_ok = c.digest_ok;
            out.signature_ok = c.signature_ok;
            out.signer_subject = c.signer_subject;
            out.signing_time = c.signing_time;
            if let Some(n) = c.note {
                doc.warn(0, format!("signature {}: {}", field, n));
            }
        } else if supported && parts.is_none() {
            // ByteRange outside the file: the digest cannot match.
            out.digest_ok = Some(false);
        }
        if out.signing_time.is_none()
            && let Some(Obj::Str(m)) = s.dict.get(b"M")
        {
            out.signing_time = meta::parse_pdf_date(&text_string(m)).utc;
        }
        let base = out.revision.unwrap_or(s.first_rev);
        out.later_revisions = ((base + 1)..=n).collect();
        if let Some(rv) = out.revision
            && let Some(r) = rep.revisions.get_mut(rv - 1)
        {
            r.signed_by.push(field.clone());
        }
        rep.signatures.push(out);
    }

    // ---------------------------------------------------------- content signals
    let signals = collect_signals(&doc);
    let mut embedded: Vec<Embedded> = Vec::new();
    let mut seen_emb: HashSet<Ref> = HashSet::new();
    for (rev, fs) in &signals.filespecs {
        let name = [&b"UF"[..], b"F"]
            .iter()
            .find_map(|k| {
                doc.deref(*rev, fs.get(k)?)
                    .and_then(|o| o.as_str().map(text_string))
            })
            .unwrap_or_default();
        let Some(ef) = fs.get(b"EF").and_then(|e| doc.get_dict(*rev, e)) else {
            continue;
        };
        for key in [&b"UF"[..], b"F"] {
            if let Some(r) = ef.get(key).and_then(Obj::as_ref)
                && seen_emb.insert(r)
                && let Some(Obj::Stream(st)) = doc.resolve(*rev, r)
            {
                embedded.push(Embedded {
                    kind: "attachment".into(),
                    name: name.clone(),
                    object: id_str(r),
                    revision: *rev,
                    offset: st.data_start,
                    length: st.data_len,
                    filters: crate::filters::filter_names(&st.dict),
                });
            }
        }
    }
    for (rev, r, st) in &signals.embedded_streams {
        if seen_emb.insert(*r) {
            embedded.push(Embedded {
                kind: "attachment".into(),
                name: String::new(),
                object: id_str(*r),
                revision: *rev,
                offset: st.data_start,
                length: st.data_len,
                filters: crate::filters::filter_names(&st.dict),
            });
        }
    }
    let attachments = embedded.len();
    if opts.embedded {
        for (rev, r, st) in &signals.images {
            embedded.push(Embedded {
                kind: "image".into(),
                name: format!("image-{}-{}", r.0, r.1),
                object: id_str(*r),
                revision: *rev,
                offset: st.data_start,
                length: st.data_len,
                filters: crate::filters::filter_names(&st.dict),
            });
        }
        rep.embedded = embedded;
    }

    // Page content of the final revision.
    let budget = Cell::new(CONTENT_OP_BUDGET);
    let mut invisible_pages = Vec::new();
    let mut over_image_pages = Vec::new();
    let mut scanned_pages = Vec::new();
    if n > 0 && !doc.encrypted {
        for (i, p) in all_pages[n - 1].iter().enumerate() {
            let st = pages::page_stats(&doc, n, p, &budget);
            if st.invisible_shows > 0 {
                invisible_pages.push((i + 1, st.invisible_shows));
            }
            if st.max_image_cover >= IMAGE_PAGE_COVER {
                if st.text_shows > 0 {
                    // First revision in which this page had text.
                    let mut first = n;
                    for k in 1..n {
                        if let Some(pk) = all_pages[k - 1].get(i)
                            && pages::page_stats(&doc, k, pk, &budget).text_shows > 0
                        {
                            first = k;
                            break;
                        }
                    }
                    over_image_pages.push((i + 1, first));
                } else {
                    scanned_pages.push(i + 1);
                }
            }
        }
        if budget.get() == 0 {
            doc.warn(
                0,
                "content analysis budget exhausted; later pages not fully examined".into(),
            );
        }
    }

    // ---------------------------------------------------------- fingerprints
    // The bundled starter DB is always used; the core may pass it again (with the local and extra
    // DBs), so databases with the same name and version are used once.
    let mut dbs = vec![fingerprint::starter_db()];
    for db in &opts.fingerprints {
        if !dbs
            .iter()
            .any(|d| d.name == db.name && d.version == db.version)
        {
            dbs.push(db.clone());
        }
    }
    let traits: Vec<RevTraits> = (1..=n)
        .map(|k| {
            let ri = &doc.revs[k - 1];
            let mut values = if k == 1 {
                header_traits(bytes, header)
            } else {
                BTreeMap::new()
            };
            values.insert(
                "xref_kind".into(),
                ri.section
                    .as_ref()
                    .map_or(XrefKind::Scan, |s| s.kind)
                    .as_str()
                    .into(),
            );
            let objstm = ri.section.as_ref().is_some_and(|s| {
                s.entries
                    .values()
                    .any(|e| matches!(e, XEntry::Compressed { .. }))
            });
            values.insert(
                "object_streams".into(),
                if objstm { "yes" } else { "no" }.into(),
            );
            RevTraits {
                revision: k,
                values,
            }
        })
        .collect();
    let strings: Vec<Strings> = rep
        .revisions
        .iter()
        .map(|r| Strings {
            revision: r.index,
            info: &r.info,
            xmp: r.xmp.as_ref(),
        })
        .collect();
    rep.fingerprints = fingerprint::match_all(&dbs, &strings, &traits);

    // ---------------------------------------------------------- findings
    let mut f: Vec<PdfFinding> = Vec::new();
    if rep.encrypted {
        f.push(PdfFinding::new(
            "ENCRYPTED_PARTIAL_ANALYSIS",
            Severity::Info,
        ));
    }
    if n > 1 {
        f.push(
            PdfFinding::new("INCREMENTAL_UPDATES", Severity::Low).with("revisions", n.to_string()),
        );
    }
    // Info vs XMP (current state = final revision).
    if let Some(last) = rep.revisions.last()
        && let Some(x) = &last.xmp
    {
        let i = &last.info;
        let pairs: [(&str, Option<&String>, Option<&String>); 2] = [
            ("Producer", i.producer.as_ref(), x.producer.as_ref()),
            ("Creator", i.creator.as_ref(), x.creator_tool.as_ref()),
        ];
        for (field, a, b) in pairs {
            if let (Some(a), Some(b)) = (a, b)
                && a.trim() != b.trim()
            {
                f.push(
                    PdfFinding::new("METADATA_INFO_XMP_MISMATCH", Severity::Medium)
                        .with("field", field)
                        .with("info", a.clone())
                        .with("xmp", b.clone()),
                );
            }
        }
        let dates: [(&str, Option<&PdfDate>, Option<&String>); 2] = [
            (
                "CreationDate",
                i.creation_date.as_ref(),
                x.create_date.as_ref(),
            ),
            ("ModDate", i.mod_date.as_ref(), x.modify_date.as_ref()),
        ];
        for (field, a, b) in dates {
            if let (Some(a), Some(b)) = (a, b)
                && let (Some(ia), Some(ib)) = (parse_pdf_instant(&a.raw), parse_xmp_instant(b))
                && !same_instant(ia, ib)
            {
                f.push(
                    PdfFinding::new("METADATA_INFO_XMP_MISMATCH", Severity::Medium)
                        .with("field", field)
                        .with("info", a.raw.clone())
                        .with("xmp", b.clone()),
                );
            }
        }
    }
    // ModDate before CreationDate.
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for r in &rep.revisions {
        let mut check = |src: &str, c: Option<&str>, m: Option<&str>, xmp: bool| {
            let (Some(c), Some(m)) = (c, m) else { return };
            let parse = |s: &str| {
                if xmp {
                    parse_xmp_instant(s)
                } else {
                    parse_pdf_instant(s)
                }
            };
            if let (Some(ic), Some(im)) = (parse(c), parse(m))
                && im.secs < ic.secs
                && seen_pairs.insert((c.to_string(), m.to_string()))
            {
                f.push(
                    PdfFinding::new("MODDATE_BEFORE_CREATIONDATE", Severity::High)
                        .with("revision", r.index.to_string())
                        .with("source", src)
                        .with("creation", c)
                        .with("modification", m),
                );
            }
        };
        check(
            "Info",
            r.info.creation_date.as_ref().map(|d| d.raw.as_str()),
            r.info.mod_date.as_ref().map(|d| d.raw.as_str()),
            false,
        );
        if let Some(x) = &r.xmp {
            check(
                "XMP",
                x.create_date.as_deref(),
                x.modify_date.as_deref(),
                true,
            );
        }
    }
    // Revision time gaps.
    let rev_time = |r: &Revision| -> Option<i64> {
        r.info
            .mod_date
            .as_ref()
            .and_then(|d| parse_pdf_instant(&d.raw))
            .or_else(|| {
                r.xmp
                    .as_ref()
                    .and_then(|x| x.modify_date.as_deref())
                    .and_then(parse_xmp_instant)
            })
            .or_else(|| {
                (r.index == 1)
                    .then(|| {
                        r.info
                            .creation_date
                            .as_ref()
                            .and_then(|d| parse_pdf_instant(&d.raw))
                    })
                    .flatten()
            })
            .map(|i| i.secs)
    };
    for w in rep.revisions.windows(2) {
        if let (Some(a), Some(b)) = (rev_time(&w[0]), rev_time(&w[1]))
            && b - a >= MIN_REVISION_GAP_SECS
        {
            f.push(
                PdfFinding::new("REVISION_TIME_GAP", Severity::Info)
                    .with("from_rev", w[0].index.to_string())
                    .with("to_rev", w[1].index.to_string())
                    .with("gap", fmt_gap(b - a)),
            );
        }
        if let (Some(a), Some(b)) = (&w[0].info.producer, &w[1].info.producer)
            && a != b
        {
            f.push(
                PdfFinding::new("PRODUCER_CHANGED_BETWEEN_REVISIONS", Severity::Medium)
                    .with("from_rev", w[0].index.to_string())
                    .with("to_rev", w[1].index.to_string())
                    .with("before", a.clone())
                    .with("after", b.clone()),
            );
        }
    }
    // Fingerprints.
    let mut labels_done: HashSet<(String, String)> = HashSet::new();
    for m in &rep.fingerprints {
        let code = match m.category.as_str() {
            "pdf_editor" => Some(("EDITOR_FINGERPRINT", Severity::Medium)),
            "online_editor" | "unlocker" => Some(("ONLINE_EDITOR_OR_UNLOCKER", Severity::High)),
            _ => None,
        };
        if let Some((code, sev)) = code
            && labels_done.insert((code.to_string(), m.label.clone()))
        {
            f.push(
                PdfFinding::new(code, sev)
                    .with("label", m.label.clone())
                    .with("revision", m.revision.to_string())
                    .with("matched_on", m.matched_on.clone())
                    .with("value", m.matched_value.clone()),
            );
        }
        if !m.trait_mismatches.is_empty() && labels_done.insert(("PSM".into(), m.label.clone())) {
            f.push(
                PdfFinding::new("PRODUCER_STRUCTURE_MISMATCH", Severity::Medium)
                    .with("label", m.label.clone())
                    .with("revision", m.revision.to_string())
                    .with("traits", m.trait_mismatches.join("; ")),
            );
        }
    }
    let mut unknown_done: HashSet<String> = HashSet::new();
    for r in &rep.revisions {
        for p in [
            r.info.producer.as_ref(),
            r.xmp.as_ref().and_then(|x| x.producer.as_ref()),
        ]
        .into_iter()
        .flatten()
        {
            let known = rep.fingerprints.iter().any(|m| &m.matched_value == p);
            if !known && !p.trim().is_empty() && unknown_done.insert(p.clone()) {
                f.push(
                    PdfFinding::new("UNKNOWN_PRODUCER", Severity::Info)
                        .with("producer", p.clone())
                        .with("revision", r.index.to_string()),
                );
            }
        }
    }
    // Structure.
    let bad: Vec<usize> = rep
        .revisions
        .iter()
        .filter(|r| !r.xref_consistent)
        .map(|r| r.index)
        .collect();
    if !bad.is_empty() {
        f.push(
            PdfFinding::new("XREF_OFFSETS_INCONSISTENT", Severity::Medium)
                .with("revisions", join(&bad)),
        );
    }
    let hybrid: Vec<usize> = rep
        .revisions
        .iter()
        .filter(|r| r.xref_kind == "hybrid")
        .map(|r| r.index)
        .collect();
    if !hybrid.is_empty() {
        f.push(PdfFinding::new("HYBRID_XREF", Severity::Low).with("revisions", join(&hybrid)));
    }
    if let Some(last) = doc.revs.last() {
        let tail = bytes.get(last.end..).unwrap_or(&[]);
        if tail.iter().any(|&b| !crate::obj::is_ws(b)) {
            f.push(
                PdfFinding::new("DATA_AFTER_FINAL_EOF", Severity::Medium)
                    .with("bytes", tail.len().to_string()),
            );
        }
    }
    // Signatures.
    for (s, found) in rep.signatures.iter().zip(sigs.iter()) {
        let signer = s.signer_subject.clone().unwrap_or_else(|| "unknown".into());
        let mut reasons = Vec::new();
        if s.digest_ok == Some(false) {
            reasons.push("digest_mismatch");
        }
        if s.signature_ok == Some(false) {
            reasons.push("signature_invalid");
        }
        if s.revision.is_none() {
            reasons.push("byterange_not_at_revision_end");
        } else if !s.covers_revision {
            reasons.push("byterange_gap_not_contents");
        }
        if !reasons.is_empty() {
            f.push(
                PdfFinding::new("SIGNATURE_INTEGRITY_FAILED", Severity::High)
                    .with("field", s.field.clone())
                    .with("reason", reasons.join(", ")),
            );
        }
        f.push(
            PdfFinding::new("SIGNATURE_PRESENT_TRUST_NOT_VERIFIED", Severity::Info)
                .with("field", s.field.clone())
                .with("signer", signer),
        );
        let changed: Vec<usize> = s
            .later_revisions
            .iter()
            .copied()
            .filter(|&j| {
                rep.revisions.get(j - 1).is_some_and(|r| {
                    r.change_kind.iter().any(|k| k == "other") || !r.pages_changed.is_empty()
                })
            })
            .collect();
        if !changed.is_empty() {
            f.push(
                PdfFinding::new("CONTENT_CHANGED_AFTER_SIGNATURE", Severity::High)
                    .with("field", s.field.clone())
                    .with("revisions", join(&changed)),
            );
        }
        if let Some(level) = s.docmdp {
            let mut viol = Vec::new();
            for &j in &s.later_revisions {
                let Some(r) = rep.revisions.get(j - 1) else {
                    continue;
                };
                let ts_only = {
                    let added: Vec<&sig::SigFound> =
                        sigs.iter().filter(|x| x.first_rev == j).collect();
                    !added.is_empty() && added.iter().all(|x| x.doc_timestamp)
                };
                let bad = r.change_kind.iter().any(|k| match (level, k.as_str()) {
                    (_, "metadata") => false,
                    (1, "signature") => !ts_only,
                    (1, _) => true,
                    (2, "signature" | "form_fill") => false,
                    (3, "signature" | "form_fill" | "annotations") => false,
                    _ => true,
                });
                if bad {
                    viol.push(j);
                }
            }
            if !viol.is_empty() {
                f.push(
                    PdfFinding::new("DOCMDP_VIOLATION", Severity::High)
                        .with("field", s.field.clone())
                        .with("level", level.to_string())
                        .with("revisions", join(&viol)),
                );
            }
        }
        let _ = found;
    }
    // Active content and attachments.
    if !signals.js.is_empty() {
        let v: Vec<&String> = signals.js.iter().take(20).collect();
        f.push(
            PdfFinding::new("JAVASCRIPT_PRESENT", Severity::Medium)
                .with("count", signals.js.len().to_string())
                .with("objects", join(&v)),
        );
    }
    if !signals.launch.is_empty() {
        let v: Vec<&String> = signals.launch.iter().take(20).collect();
        f.push(
            PdfFinding::new("LAUNCH_ACTION", Severity::High)
                .with("count", signals.launch.len().to_string())
                .with("objects", join(&v)),
        );
    }
    if attachments > 0 {
        f.push(
            PdfFinding::new("EMBEDDED_FILES", Severity::Low).with("count", attachments.to_string()),
        );
    }
    for (page, count) in invisible_pages.iter().take(MAX_PAGE_FINDINGS) {
        f.push(
            PdfFinding::new("INVISIBLE_TEXT", Severity::Medium)
                .with("page", page.to_string())
                .with("count", count.to_string()),
        );
    }
    for (page, first) in over_image_pages.iter().take(MAX_PAGE_FINDINGS) {
        f.push(
            PdfFinding::new("TEXT_OVER_SCANNED_IMAGE", Severity::Medium)
                .with("page", page.to_string())
                .with("text_revision", first.to_string()),
        );
    }
    if !scanned_pages.is_empty() {
        let v: Vec<usize> = scanned_pages.iter().copied().take(200).collect();
        f.push(
            PdfFinding::new("SCANNED_PAGES_STRUCTURE_ONLY", Severity::Info)
                .with("pages", join(&v))
                .with("count", scanned_pages.len().to_string()),
        );
    }
    // Fonts added later and used by page content.
    let mut pf_cache: HashMap<usize, (HashSet<Ref>, HashSet<String>)> = HashMap::new();
    for font in &rep.fonts {
        let k = font.first_revision;
        if k < 2 {
            continue;
        }
        let Some(r) = parse_id(&font.object) else {
            continue;
        };
        let cur = pf_cache
            .entry(k)
            .or_insert_with(|| page_fonts(&doc, k))
            .0
            .contains(&r);
        if !cur {
            continue;
        }
        let base = split_subset(&font.name).1;
        let before = pf_cache
            .entry(k - 1)
            .or_insert_with(|| page_fonts(&doc, k - 1))
            .1
            .contains(&base);
        if !before {
            f.push(
                PdfFinding::new("FONT_ADDED_IN_LATER_REVISION", Severity::Medium)
                    .with("font", font.name.clone())
                    .with("revision", k.to_string()),
            );
        }
    }
    f.sort_by_key(|x| std::cmp::Reverse(x.severity));
    rep.findings = f;

    // ---------------------------------------------------------- object tree
    if opts.object_tree {
        rep.object_tree = object_tree(&doc);
    }
    rep.warnings = doc
        .take_warnings()
        .into_iter()
        .map(|(offset, message)| ParseWarning { offset, message })
        .collect();
    rep
}

fn parse_id(s: &str) -> Option<Ref> {
    let (a, b) = s.split_once(' ')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

fn obj_json(o: &Obj, depth: usize) -> serde_json::Value {
    use serde_json::{Value, json};
    if depth > 8 {
        return Value::String("…".into());
    }
    match o {
        Obj::Null => Value::Null,
        Obj::Bool(b) => Value::Bool(*b),
        Obj::Int(i) => json!(i),
        Obj::Real(r) => json!(r),
        Obj::Name(n) => Value::String(format!("/{}", String::from_utf8_lossy(n))),
        Obj::Str(s) => {
            if s.len() <= 256 && s.iter().all(|&c| (0x20..0x7F).contains(&c)) {
                Value::String(String::from_utf8_lossy(s).into_owned())
            } else {
                Value::String(format!("<{}>", hex(&s[..s.len().min(256)])))
            }
        }
        Obj::Array(a) => Value::Array(
            a.iter()
                .take(1000)
                .map(|x| obj_json(x, depth + 1))
                .collect(),
        ),
        Obj::Dict(d) => {
            let mut m = serde_json::Map::new();
            for (k, v) in d.0.iter().take(1000) {
                m.insert(
                    String::from_utf8_lossy(k).into_owned(),
                    obj_json(v, depth + 1),
                );
            }
            Value::Object(m)
        }
        Obj::Ref(n, g) => Value::String(format!("{} {} R", n, g)),
        Obj::Stream(s) => obj_json(&Obj::Dict(s.dict.clone()), depth),
    }
}

fn object_tree(doc: &Doc) -> serde_json::Value {
    use serde_json::json;
    let mut revs = Vec::new();
    for k in 1..=doc.revs.len() {
        let mut objs = Vec::new();
        for (num, e) in doc.section_entries(k) {
            if num == 0 || e == XEntry::Free {
                continue;
            }
            let Some(g) = doc.gen_of(k, num) else {
                continue;
            };
            let Some((o, loc)) = doc.resolve_loc(k, (num, g)) else {
                continue;
            };
            let (offset, in_stream) = match loc {
                Loc::File(off) => (off as u64, serde_json::Value::Null),
                Loc::InStream(off, stm) => (off as u64, json!(stm)),
            };
            let ty = o
                .as_dict()
                .and_then(|d| d.name(b"Type"))
                .map(|t| String::from_utf8_lossy(t).into_owned())
                .unwrap_or_else(|| {
                    match &o {
                        Obj::Stream(_) => "stream",
                        Obj::Dict(_) => "dict",
                        Obj::Array(_) => "array",
                        _ => "value",
                    }
                    .into()
                });
            let mut entry = json!({"id": id_str((num, g)), "offset": offset, "type": ty, "dict": obj_json(&o, 0)});
            if !in_stream.is_null() {
                entry["in_object_stream"] = in_stream;
            }
            if let Obj::Stream(s) = &o {
                entry["stream"] = json!({"offset": s.data_start, "length": s.data_len});
            }
            objs.push(entry);
        }
        revs.push(json!({"objects": objs}));
    }
    json!({ "revisions": revs })
}
