//! Document information dictionary, PDF dates and XMP metadata (tiny namespace-aware XML reader,
//! no external dependency, depth and size limited).

use crate::doc::Doc;
use crate::model::{InfoDict, PdfDate, Xmp};
use crate::obj::{Obj, id_str, text_string};
use std::collections::BTreeMap;

const MAX_XMP: usize = 16 * 1024 * 1024;
const MAX_XML_DEPTH: usize = 64;
const MAX_XML_NODES: usize = 200_000;

pub fn info(doc: &Doc, rev: usize) -> InfoDict {
    let mut out = InfoDict::default();
    let Some(t) = doc.trailer(rev) else {
        return out;
    };
    let Some(iref) = t.get(b"Info") else {
        return out;
    };
    out.object = iref.as_ref().map(id_str);
    let Some(d) = doc.get_dict(rev, iref) else {
        return out;
    };
    if doc.encrypted {
        // Strings are encrypted: report only that an Info dictionary exists.
        return out;
    }
    let s = |k: &[u8]| -> Option<String> {
        match doc.deref(rev, d.get(k)?)? {
            Obj::Str(b) => Some(text_string(&b)),
            Obj::Name(n) => Some(String::from_utf8_lossy(&n).into_owned()),
            _ => None,
        }
    };
    out.producer = s(b"Producer");
    out.creator = s(b"Creator");
    out.title = s(b"Title");
    out.author = s(b"Author");
    out.creation_date = s(b"CreationDate").map(|r| parse_pdf_date(&r));
    out.mod_date = s(b"ModDate").map(|r| parse_pdf_date(&r));
    out
}

/// Civil date → days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Seconds since the epoch → "YYYY-MM-DDTHH:MM:SSZ".
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// A parsed timestamp: epoch seconds (UTC), whether the zone was given, and how many fields
/// were present (precision: 1=year … 6=seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instant {
    pub secs: i64,
    pub tz_known: bool,
    pub precision: u8,
    /// Wall-clock seconds as written (before applying the offset).
    pub local: i64,
}

fn civil_secs(f: &[i64; 6]) -> Option<i64> {
    let [y, mo, d, h, mi, s] = *f;
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) {
        return None;
    }
    if !(0..=23).contains(&h) || !(0..=59).contains(&mi) || !(0..=60).contains(&s) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s)
}

/// PDF date "D:YYYYMMDDHHmmSSOHH'mm'" (all but the year optional).
pub fn parse_pdf_instant(raw: &str) -> Option<Instant> {
    let s = raw.trim();
    let s = s.strip_prefix("D:").unwrap_or(s);
    let b = s.as_bytes();
    let mut f = [0i64, 1, 1, 0, 0, 0];
    let widths = [4usize, 2, 2, 2, 2, 2];
    let mut pos = 0;
    let mut precision = 0u8;
    for (i, w) in widths.iter().enumerate() {
        let Some(chunk) = b.get(pos..pos + w) else {
            break;
        };
        if !chunk.iter().all(u8::is_ascii_digit) {
            break;
        }
        f[i] = std::str::from_utf8(chunk).ok()?.parse().ok()?;
        pos += w;
        precision += 1;
    }
    if precision == 0 {
        return None;
    }
    let local = civil_secs(&f)?;
    let rest = &b[pos..];
    let (tz_known, offset) = match rest.first() {
        Some(b'Z') => (true, 0),
        Some(&c @ (b'+' | b'-')) => {
            let digits: Vec<u8> = rest[1..]
                .iter()
                .copied()
                .filter(u8::is_ascii_digit)
                .collect();
            let hh: i64 = std::str::from_utf8(digits.get(0..2)?).ok()?.parse().ok()?;
            let mm: i64 = digits
                .get(2..4)
                .and_then(|d| std::str::from_utf8(d).ok())
                .and_then(|d| d.parse().ok())
                .unwrap_or(0);
            if hh > 23 || mm > 59 {
                return None;
            }
            let o = hh * 3600 + mm * 60;
            (true, if c == b'-' { -o } else { o })
        }
        _ => (false, 0),
    };
    Some(Instant {
        secs: local - offset,
        tz_known,
        precision,
        local,
    })
}

pub fn parse_pdf_date(raw: &str) -> PdfDate {
    let i = parse_pdf_instant(raw);
    PdfDate {
        raw: raw.to_string(),
        utc: i.map(|i| iso_utc(i.secs)),
        tz_missing: i.is_some_and(|i| !i.tz_known),
    }
}

/// ISO 8601 / XMP date: "YYYY", "YYYY-MM", "YYYY-MM-DD", "YYYY-MM-DDThh:mm[:ss[.s+]][TZD]".
pub fn parse_xmp_instant(raw: &str) -> Option<Instant> {
    let s = raw.trim().as_bytes();
    let num = |a: usize, l: usize| -> Option<i64> {
        let c = s.get(a..a + l)?;
        if !c.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(c).ok()?.parse().ok()
    };
    let mut f = [num(0, 4)?, 1, 1, 0, 0, 0];
    let mut precision = 1u8;
    let mut pos = 4;
    if s.get(4) == Some(&b'-') {
        f[1] = num(5, 2)?;
        precision = 2;
        pos = 7;
        if s.get(7) == Some(&b'-') {
            f[2] = num(8, 2)?;
            precision = 3;
            pos = 10;
            if s.get(10) == Some(&b'T') {
                f[3] = num(11, 2)?;
                f[4] = num(14, 2)?;
                precision = 5;
                pos = 16;
                if s.get(16) == Some(&b':') {
                    f[5] = num(17, 2)?;
                    precision = 6;
                    pos = 19;
                    if s.get(19) == Some(&b'.') {
                        pos = 20;
                        while s.get(pos).is_some_and(u8::is_ascii_digit) {
                            pos += 1;
                        }
                    }
                }
            }
        }
    }
    let local = civil_secs(&f)?;
    let (tz_known, offset) = match s.get(pos) {
        Some(b'Z') => (true, 0),
        Some(&c @ (b'+' | b'-')) => {
            let hh = num(pos + 1, 2)?;
            let mm = if s.get(pos + 3) == Some(&b':') {
                num(pos + 4, 2)?
            } else {
                num(pos + 3, 2).unwrap_or(0)
            };
            let o = hh * 3600 + mm * 60;
            (true, if c == b'-' { -o } else { o })
        }
        _ => (false, 0),
    };
    Some(Instant {
        secs: local - offset,
        tz_known,
        precision,
        local,
    })
}

/// Same moment, compared at the coarser precision of the two; when either has no zone, wall clocks
/// are compared instead.
pub fn same_instant(a: Instant, b: Instant) -> bool {
    let p = a.precision.min(b.precision);
    let trunc = |v: i64| -> i64 {
        match p {
            6 => v,
            5 => v.div_euclid(60),
            4 => v.div_euclid(3600),
            _ => v.div_euclid(86_400),
        }
    };
    if p < 4 {
        // Date-only: compare the written dates.
        return trunc(a.local) == trunc(b.local);
    }
    if a.tz_known && b.tz_known {
        trunc(a.secs) == trunc(b.secs)
    } else {
        trunc(a.local) == trunc(b.local) || trunc(a.secs) == trunc(b.secs)
    }
}

// ---------------------------------------------------------------- XMP

#[derive(Debug, Default, Clone)]
pub struct Element {
    pub ns: String,
    pub local: String,
    pub attrs: Vec<(String, String, String)>,
    pub children: Vec<usize>,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct XmlDoc {
    pub nodes: Vec<Element>,
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let rep = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match rep {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn split_qname(q: &str) -> (&str, &str) {
    match q.split_once(':') {
        Some((p, l)) => (p, l),
        None => ("", q),
    }
}

/// Parses XML into a flat node list (node 0 = virtual root). Tolerant: stops at the first error
/// and keeps what was read.
pub fn parse_xml(src: &str) -> XmlDoc {
    let mut doc = XmlDoc {
        nodes: vec![Element::default()],
    };
    let mut stack: Vec<(usize, Vec<(String, String)>)> = vec![(0, Vec::new())];
    let b = src.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] != b'<' {
            let j = src[i..].find('<').map_or(b.len(), |k| i + k);
            if let Some((top, _)) = stack.last()
                && let Some(n) = doc.nodes.get_mut(*top)
            {
                n.text.push_str(&unescape(&src[i..j]));
            }
            i = j;
            continue;
        }
        let rest = &src[i..];
        if rest.starts_with("<!--") {
            i = rest.find("-->").map_or(b.len(), |k| i + k + 3);
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let end = rest.find("]]>").unwrap_or(rest.len());
            if let Some((top, _)) = stack.last()
                && let Some(n) = doc.nodes.get_mut(*top)
            {
                n.text.push_str(rest.get(9..end).unwrap_or(""));
            }
            i += (end + 3).min(rest.len());
            continue;
        }
        if rest.starts_with("<?") || rest.starts_with("<!") {
            i = rest.find('>').map_or(b.len(), |k| i + k + 1);
            continue;
        }
        let Some(close) = tag_end(rest) else { break };
        let tag = &rest[1..close];
        i += close + 1;
        if let Some(name) = tag.strip_prefix('/') {
            let _ = name;
            if stack.len() > 1 {
                stack.pop();
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let (qname, attr_src) = match tag.find(|c: char| c.is_whitespace()) {
            Some(k) => (&tag[..k], &tag[k..]),
            None => (tag, ""),
        };
        let raw_attrs = parse_attrs(attr_src);
        let mut scope: Vec<(String, String)> =
            stack.last().map(|s| s.1.clone()).unwrap_or_default();
        for (k, v) in &raw_attrs {
            if k == "xmlns" {
                scope.push((String::new(), v.clone()));
            } else if let Some(p) = k.strip_prefix("xmlns:") {
                scope.push((p.to_string(), v.clone()));
            }
        }
        let lookup = |p: &str| -> String {
            scope
                .iter()
                .rev()
                .find(|(pp, _)| pp == p)
                .map(|(_, u)| u.clone())
                .unwrap_or_else(|| p.to_string())
        };
        let (p, l) = split_qname(qname);
        let mut el = Element {
            ns: lookup(p),
            local: l.to_string(),
            ..Element::default()
        };
        for (k, v) in raw_attrs {
            if k == "xmlns" || k.starts_with("xmlns:") {
                continue;
            }
            let (ap, al) = split_qname(&k);
            let ns = if ap.is_empty() {
                String::new()
            } else {
                lookup(ap)
            };
            el.attrs.push((ns, al.to_string(), v));
        }
        if doc.nodes.len() >= MAX_XML_NODES {
            break;
        }
        let idx = doc.nodes.len();
        doc.nodes.push(el);
        if let Some((top, _)) = stack.last()
            && let Some(n) = doc.nodes.get_mut(*top)
        {
            n.children.push(idx);
        }
        if !self_closing {
            if stack.len() >= MAX_XML_DEPTH {
                break;
            }
            stack.push((idx, scope));
        }
    }
    doc
}

fn tag_end(s: &str) -> Option<usize> {
    let mut q: Option<u8> = None;
    for (i, c) in s.bytes().enumerate().skip(1) {
        match q {
            Some(qc) if c == qc => q = None,
            Some(_) => {}
            None if c == b'"' || c == b'\'' => q = Some(c),
            None if c == b'>' => return Some(i),
            None => {}
        }
    }
    None
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim_start();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let body = &after[1..];
        let Some(end) = body.find(q) else { break };
        out.push((key, unescape(&body[..end])));
        rest = body[end + 1..].trim_start();
    }
    out
}

pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
pub const NS_PDF: &str = "http://ns.adobe.com/pdf/1.3/";
pub const NS_MM: &str = "http://ns.adobe.com/xap/1.0/mm/";
pub const NS_EVT: &str = "http://ns.adobe.com/xap/1.0/sType/ResourceEvent#";

impl XmlDoc {
    /// Value of property ns:local: an attribute of any element, or the text of an element.
    pub fn prop(&self, ns: &str, local: &str) -> Option<String> {
        for n in &self.nodes {
            for (ans, al, v) in &n.attrs {
                if ans == ns && al == local {
                    return Some(v.trim().to_string());
                }
            }
            if n.ns == ns && n.local == local {
                let t = self.text_deep(n, 0);
                let t = t.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    }

    fn text_deep(&self, n: &Element, depth: usize) -> String {
        if depth > 8 {
            return String::new();
        }
        if !n.text.trim().is_empty() {
            return n.text.clone();
        }
        for &c in &n.children {
            if let Some(ch) = self.nodes.get(c) {
                let t = self.text_deep(ch, depth + 1);
                if !t.trim().is_empty() {
                    return t;
                }
            }
        }
        String::new()
    }

    fn history(&self) -> Vec<BTreeMap<String, String>> {
        let mut out = Vec::new();
        for n in &self.nodes {
            if !(n.ns == NS_MM && n.local == "History") {
                continue;
            }
            // History → Seq → li*
            let mut lis = Vec::new();
            for &c in &n.children {
                if let Some(seq) = self.nodes.get(c) {
                    for &l in &seq.children {
                        if let Some(li) = self.nodes.get(l)
                            && li.ns == NS_RDF
                            && li.local == "li"
                        {
                            lis.push(li);
                        }
                    }
                }
            }
            for li in lis {
                let mut m = BTreeMap::new();
                for (ns, l, v) in &li.attrs {
                    if ns == NS_EVT {
                        m.insert(l.clone(), v.clone());
                    }
                }
                let mut kids: Vec<&Element> = li
                    .children
                    .iter()
                    .filter_map(|&c| self.nodes.get(c))
                    .collect();
                // <rdf:li><rdf:Description stEvt:.../></rdf:li>
                let mut more = Vec::new();
                for k in &kids {
                    if k.ns == NS_RDF && k.local == "Description" {
                        for (ns, l, v) in &k.attrs {
                            if ns == NS_EVT {
                                m.insert(l.clone(), v.clone());
                            }
                        }
                        more.extend(k.children.iter().filter_map(|&c| self.nodes.get(c)));
                    }
                }
                kids.extend(more);
                for k in kids {
                    if k.ns == NS_EVT {
                        m.insert(k.local.clone(), k.text.trim().to_string());
                    }
                }
                if !m.is_empty() {
                    out.push(m);
                }
                if out.len() >= 1000 {
                    return out;
                }
            }
        }
        out
    }
}

pub fn parse_xmp_bytes(raw: &[u8]) -> Option<Xmp> {
    if raw.len() > MAX_XMP {
        return None;
    }
    let text = if raw.starts_with(&[0xFE, 0xFF]) || raw.starts_with(&[0xFF, 0xFE]) {
        let be = raw[0] == 0xFE;
        let u: Vec<u16> = raw[2..]
            .chunks(2)
            .map(|c| {
                let (a, b) = (c[0], *c.get(1).unwrap_or(&0));
                if be {
                    u16::from(a) << 8 | u16::from(b)
                } else {
                    u16::from(b) << 8 | u16::from(a)
                }
            })
            .collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    let x = parse_xml(&text);
    if x.nodes.len() <= 1 {
        return None;
    }
    Some(Xmp {
        producer: x.prop(NS_PDF, "Producer"),
        creator_tool: x.prop(NS_XMP, "CreatorTool"),
        create_date: x.prop(NS_XMP, "CreateDate"),
        modify_date: x.prop(NS_XMP, "ModifyDate"),
        metadata_date: x.prop(NS_XMP, "MetadataDate"),
        document_id: x.prop(NS_MM, "DocumentID"),
        instance_id: x.prop(NS_MM, "InstanceID"),
        history: x.history(),
    })
}

/// XMP of the catalog's /Metadata stream in revision `rev`.
pub fn xmp(doc: &Doc, rev: usize) -> Option<Xmp> {
    let cat = doc.catalog(rev)?;
    let m = doc.deref(rev, cat.get(b"Metadata")?)?;
    let st = m.as_stream()?;
    let data = doc.decode_stream(st, "XMP metadata")?;
    parse_xmp_bytes(&data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_dates() {
        let d = parse_pdf_date("D:20260302091500-05'00'");
        assert_eq!(d.utc.as_deref(), Some("2026-03-02T14:15:00Z"));
        assert!(!d.tz_missing);
        let d = parse_pdf_date("D:2026");
        assert_eq!(d.utc.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert!(d.tz_missing);
        assert_eq!(parse_pdf_date("D:20261399").utc, None);
        assert_eq!(parse_pdf_date("garbage").utc, None);
        let d = parse_pdf_date("D:20240229235959Z");
        assert_eq!(d.utc.as_deref(), Some("2024-02-29T23:59:59Z"));
        assert_eq!(parse_pdf_date("D:20230229").utc, None);
    }

    #[test]
    fn xmp_dates_compare() {
        let a = parse_pdf_instant("D:20260302091500-05'00'").unwrap();
        let b = parse_xmp_instant("2026-03-02T09:15:00-05:00").unwrap();
        let c = parse_xmp_instant("2026-03-02T14:15Z").unwrap();
        let d = parse_xmp_instant("2026-03-03T09:15:00-05:00").unwrap();
        assert!(same_instant(a, b));
        assert!(same_instant(a, c));
        assert!(!same_instant(a, d));
        assert!(parse_xmp_instant("2026-13-01").is_none());
    }

    #[test]
    fn xmp_parse() {
        let x = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:pdf="http://ns.adobe.com/pdf/1.3/" pdf:Producer="Lib &amp; Co"/>
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:mm="http://ns.adobe.com/xap/1.0/mm/"
  xmlns:e="http://ns.adobe.com/xap/1.0/sType/ResourceEvent#">
<xmp:CreateDate>2026-03-02T09:15:00-05:00</xmp:CreateDate><xmp:CreatorTool>Writer</xmp:CreatorTool>
<mm:History><rdf:Seq><rdf:li e:action="created" e:softwareAgent="A"/>
<rdf:li rdf:parseType="Resource"><e:action>saved</e:action><e:when>2026-03-03</e:when></rdf:li></rdf:Seq></mm:History>
</rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;
        let m = parse_xmp_bytes(x.as_bytes()).unwrap();
        assert_eq!(m.producer.as_deref(), Some("Lib & Co"));
        assert_eq!(m.creator_tool.as_deref(), Some("Writer"));
        assert_eq!(m.create_date.as_deref(), Some("2026-03-02T09:15:00-05:00"));
        assert_eq!(m.history.len(), 2);
        assert_eq!(m.history[1]["action"], "saved");
        assert_eq!(m.history[0]["softwareAgent"], "A");
    }

    #[test]
    fn xml_garbage_never_panics() {
        for s in [
            "<",
            "<a",
            "<a b='>",
            "<<<>>>",
            "&#xFFFFFFFF;",
            "<a:b xmlns:a='x'><![CDATA[",
            "<é/>&é;",
        ] {
            let _ = parse_xml(s);
        }
        let deep = "<a>".repeat(10_000);
        assert!(parse_xml(&deep).nodes.len() <= MAX_XML_DEPTH + 2);
    }
}
