//! Raw PDF tokenizer and object parser. Works on byte slices, keeps byte offsets, never panics:
//! every index is checked, nesting is depth-limited and malformed input yields `None`.

/// Maximum nesting of arrays/dictionaries accepted by the parser.
pub const MAX_DEPTH: usize = 64;

pub type Ref = (u32, u16);

#[derive(Clone, Debug, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Dict),
    Ref(u32, u16),
    Stream(Box<Stream>),
}

/// A stream: its dictionary plus the location of its (still encoded) data. `data_start` is relative
/// to the buffer the stream was parsed from (the file, for top-level objects).
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    pub data_start: u64,
    pub data_len: u64,
    /// The declared /Length did not match the data actually found before `endstream`.
    pub length_repaired: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dict(pub Vec<(Vec<u8>, Obj)>);

impl Dict {
    /// Last value for `key` (later duplicates win, as in most readers).
    pub fn get(&self, key: &[u8]) -> Option<&Obj> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn has(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }
    pub fn name(&self, key: &[u8]) -> Option<&[u8]> {
        self.get(key).and_then(Obj::as_name)
    }
    pub fn int(&self, key: &[u8]) -> Option<i64> {
        self.get(key).and_then(Obj::as_int)
    }
    pub fn type_is(&self, t: &[u8]) -> bool {
        self.name(b"Type") == Some(t)
    }
    pub fn keys(&self) -> impl Iterator<Item = &[u8]> {
        self.0.iter().map(|(k, _)| k.as_slice())
    }
}

impl Obj {
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) => Some(d),
            Obj::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }
    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Obj::Stream(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Obj::Int(i) => Some(*i),
            Obj::Real(r) if r.is_finite() && r.fract() == 0.0 && r.abs() < 9.0e15 => {
                Some(*r as i64)
            }
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Obj::Int(i) => Some(*i as f64),
            Obj::Real(r) if r.is_finite() => Some(*r),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            Obj::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Obj]> {
        match self {
            Obj::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_ref(&self) -> Option<Ref> {
        match self {
            Obj::Ref(n, g) => Some((*n, *g)),
            _ => None,
        }
    }
}

pub fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}
pub fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}
pub fn is_regular(b: u8) -> bool {
    !is_ws(b) && !is_delim(b)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    ArrOpen,
    ArrClose,
    DictOpen,
    DictClose,
    Keyword(Vec<u8>),
}

/// Byte-level lexer. `pos` is always within `0..=data.len()`.
pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Lexer {
            data,
            pos: pos.min(data.len()),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// Skips whitespace and comments.
    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Next token with its start offset, or None at end of data.
    pub fn next_tok(&mut self) -> Option<(usize, Tok)> {
        self.skip_ws();
        let start = self.pos;
        let b = self.peek()?;
        let tok = match b {
            b'[' => {
                self.pos += 1;
                Tok::ArrOpen
            }
            b']' => {
                self.pos += 1;
                Tok::ArrClose
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Tok::DictOpen
                } else {
                    self.pos += 1;
                    Tok::Str(self.hex_string())
                }
            }
            b'>' => {
                if self.data.get(self.pos + 1) == Some(&b'>') {
                    self.pos += 2;
                    Tok::DictClose
                } else {
                    self.pos += 1;
                    Tok::Keyword(b">".to_vec())
                }
            }
            b'(' => {
                self.pos += 1;
                Tok::Str(self.literal_string())
            }
            b'/' => {
                self.pos += 1;
                Tok::Name(self.name())
            }
            b')' | b'{' | b'}' => {
                self.pos += 1;
                Tok::Keyword(vec![b])
            }
            _ => {
                while let Some(c) = self.peek() {
                    if !is_regular(c) {
                        break;
                    }
                    self.pos += 1;
                }
                let word = self.data.get(start..self.pos).unwrap_or(&[]);
                if word.is_empty() {
                    // Should not happen (b is regular), but never loop forever.
                    self.pos += 1;
                    return Some((start, Tok::Keyword(vec![b])));
                }
                number(word).unwrap_or_else(|| Tok::Keyword(word.to_vec()))
            }
        };
        Some((start, tok))
    }

    fn name(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(c) = self.peek() {
            if !is_regular(c) {
                break;
            }
            self.pos += 1;
            if c == b'#' {
                let h = self.data.get(self.pos..self.pos + 2).and_then(hex2);
                if let Some(v) = h {
                    out.push(v);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(c);
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == b'>' {
                break;
            }
            if let Some(v) = hexval(c) {
                match hi.take() {
                    Some(h) => out.push(h << 4 | v),
                    None => hi = Some(v),
                }
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        out
    }

    fn literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            self.pos += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\\' => {
                    let Some(e) = self.peek() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v: u32 = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xFF) as u8);
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(c),
            }
        }
        out
    }

    /// Skips inline image data after the `ID` operator (content streams only).
    pub fn skip_inline_image(&mut self) {
        // One whitespace byte follows ID, then binary data until whitespace + "EI" + delimiter/EOF.
        self.pos = (self.pos + 1).min(self.data.len());
        let mut i = self.pos;
        while i + 1 < self.data.len() {
            if self.data[i] == b'E'
                && self.data[i + 1] == b'I'
                && i > 0
                && is_ws(self.data[i - 1])
                && self.data.get(i + 2).is_none_or(|&c| !is_regular(c))
            {
                self.pos = i + 2;
                return;
            }
            i += 1;
        }
        self.pos = self.data.len();
    }
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn hex2(s: &[u8]) -> Option<u8> {
    let a = hexval(*s.first()?)?;
    let b = hexval(*s.get(1)?)?;
    Some(a << 4 | b)
}

/// Parses a PDF numeric token. Integers that overflow i64 become reals.
pub fn number(w: &[u8]) -> Option<Tok> {
    let (sign, digits) = match w.first()? {
        b'+' => (1.0, w.get(1..)?),
        b'-' => (-1.0, w.get(1..)?),
        _ => (1.0, w),
    };
    if digits.is_empty() {
        return None;
    }
    let mut dot = false;
    for &c in digits {
        match c {
            b'0'..=b'9' => {}
            b'.' if !dot => dot = true,
            _ => return None,
        }
    }
    if digits == b"." {
        return None;
    }
    let s = std::str::from_utf8(digits).ok()?;
    if !dot && let Ok(v) = s.parse::<i64>() {
        return Some(Tok::Int(if sign < 0.0 { -v } else { v }));
    }
    let v: f64 = if let Some(rest) = s.strip_prefix('.') {
        format!("0.{}", rest).parse().ok()?
    } else if let Some(stripped) = s.strip_suffix('.') {
        stripped.parse().ok()?
    } else {
        s.parse().ok()?
    };
    Some(Tok::Real(sign * v))
}

/// Recursive-descent object parser over a lexer.
pub struct Parser<'a> {
    pub lex: Lexer<'a>,
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Parser {
            lex: Lexer::new(data, pos),
        }
    }

    /// Parses one direct object (references are kept as `Obj::Ref`).
    pub fn object(&mut self, depth: usize) -> Option<Obj> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (_, tok) = self.lex.next_tok()?;
        self.object_from(tok, depth)
    }

    pub fn object_from(&mut self, tok: Tok, depth: usize) -> Option<Obj> {
        Some(match tok {
            Tok::Int(i) => {
                // "N G R" lookahead.
                let save = self.lex.pos;
                if (0..=i64::from(u32::MAX)).contains(&i)
                    && let Some((_, Tok::Int(g))) = self.lex.next_tok()
                    && (0..=65535).contains(&g)
                    && let Some((_, Tok::Keyword(k))) = self.lex.next_tok()
                    && k == b"R"
                {
                    return Some(Obj::Ref(i as u32, g as u16));
                }
                self.lex.pos = save;
                Obj::Int(i)
            }
            Tok::Real(r) => Obj::Real(r),
            Tok::Name(n) => Obj::Name(n),
            Tok::Str(s) => Obj::Str(s),
            Tok::ArrOpen => {
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut v = Vec::new();
                loop {
                    let (_, t) = self.lex.next_tok()?;
                    match t {
                        Tok::ArrClose => break,
                        Tok::DictClose => return None,
                        Tok::Keyword(ref k) if is_structural_kw(k) => return None,
                        t => v.push(self.object_from(t, depth + 1)?),
                    }
                }
                Obj::Array(v)
            }
            Tok::DictOpen => {
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut d = Vec::new();
                loop {
                    let (_, t) = self.lex.next_tok()?;
                    match t {
                        Tok::DictClose => break,
                        Tok::Name(k) => {
                            let save = self.lex.pos;
                            let (_, vt) = self.lex.next_tok()?;
                            if vt == Tok::DictClose {
                                // Key without value: tolerated as null, like most readers.
                                d.push((k, Obj::Null));
                                break;
                            }
                            if let Tok::Keyword(ref kw) = vt
                                && is_structural_kw(kw)
                            {
                                self.lex.pos = save;
                                return None;
                            }
                            let v = self.object_from(vt, depth + 1)?;
                            d.push((k, v));
                        }
                        _ => return None,
                    }
                }
                Obj::Dict(Dict(d))
            }
            Tok::Keyword(k) => match k.as_slice() {
                b"true" => Obj::Bool(true),
                b"false" => Obj::Bool(false),
                b"null" => Obj::Null,
                _ => return None,
            },
            Tok::ArrClose | Tok::DictClose => return None,
        })
    }
}

fn is_structural_kw(k: &[u8]) -> bool {
    matches!(
        k,
        b"obj" | b"endobj" | b"stream" | b"endstream" | b"xref" | b"trailer" | b"startxref"
    )
}

/// Result of parsing "N G obj ... endobj" at a position.
#[derive(Clone, Debug)]
pub struct IndirectObj {
    pub num: u32,
    pub gen_: u16,
    pub obj: Obj,
    /// Offset of the first byte of "N".
    pub start: usize,
    /// Offset just after `endobj` (or after the object/stream when `endobj` is missing).
    pub end: usize,
    pub endobj_missing: bool,
}

/// Reads "N G obj" at `pos` (exactly, no leading whitespace skipped by the caller's choice).
pub fn read_obj_header(data: &[u8], pos: usize) -> Option<(u32, u16, usize)> {
    let mut lx = Lexer::new(data, pos);
    let (s, n) = lx.next_tok()?;
    if s != pos {
        return None;
    }
    let (_, g) = lx.next_tok()?;
    let (_, k) = lx.next_tok()?;
    match (n, g, k) {
        (Tok::Int(n), Tok::Int(g), Tok::Keyword(k))
            if k == b"obj"
                && (0..=i64::from(u32::MAX)).contains(&n)
                && (0..=65535).contains(&g) =>
        {
            Some((n as u32, g as u16, lx.pos))
        }
        _ => None,
    }
}

/// Context for `parse_indirect`: how to resolve an indirect /Length and where `endstream`
/// keywords are (sorted offsets; avoids quadratic searches on hostile files).
pub struct ObjCtx<'c> {
    pub resolve_len: &'c dyn Fn(Ref) -> Option<i64>,
    pub endstreams: Option<&'c [usize]>,
}

impl ObjCtx<'_> {
    fn next_endstream(&self, data: &[u8], from: usize) -> Option<usize> {
        match self.endstreams {
            Some(v) => {
                let i = v.partition_point(|&p| p < from);
                v.get(i).copied()
            }
            None => find(data, b"endstream", from),
        }
    }
}

/// Parses an indirect object at `pos`. On failure returns the furthest offset the parser
/// reached, so callers can account for the work done.
pub fn parse_indirect(data: &[u8], pos: usize, ctx: &ObjCtx) -> Result<IndirectObj, usize> {
    let (num, gen_, after) = read_obj_header(data, pos).ok_or(pos)?;
    let mut p = Parser::new(data, after);
    let obj = match p.object(0) {
        Some(o) => o,
        None => return Err(p.lex.pos),
    };
    parse_indirect_rest(data, pos, num, gen_, obj, p.lex.pos, ctx).ok_or(p.lex.pos)
}

fn parse_indirect_rest(
    data: &[u8],
    pos: usize,
    num: u32,
    gen_: u16,
    obj: Obj,
    after_obj: usize,
    ctx: &ObjCtx,
) -> Option<IndirectObj> {
    let mut lx = Lexer::new(data, after_obj);
    let save = lx.pos;
    let next = lx.next_tok();
    let (obj, end_pos, endobj_missing) = match next {
        Some((kpos, Tok::Keyword(k))) if k == b"stream" => {
            let Obj::Dict(dict) = obj else {
                return None;
            };
            // Data starts after the EOL following "stream" (CRLF or LF; a lone CR is tolerated).
            let mut ds = kpos + 6;
            match (data.get(ds), data.get(ds + 1)) {
                (Some(b'\r'), Some(b'\n')) => ds += 2,
                (Some(b'\n'), _) | (Some(b'\r'), _) => ds += 1,
                _ => {}
            }
            let declared = match dict.get(b"Length") {
                Some(Obj::Int(l)) => Some(*l),
                Some(Obj::Ref(n, g)) => (ctx.resolve_len)((*n, *g)),
                _ => None,
            };
            let mut len_ok = None;
            if let Some(l) = declared
                && l >= 0
                && let Some(e) = ds.checked_add(l as usize)
                && e <= data.len()
            {
                let mut l2 = Lexer::new(data, e);
                l2.skip_ws();
                if data.get(l2.pos..l2.pos + 9) == Some(b"endstream") {
                    len_ok = Some((l as usize, l2.pos + 9));
                }
            }
            let (dlen, after_es, repaired) = match len_ok {
                Some((l, a)) => (l, a, false),
                None => {
                    let found = ctx.next_endstream(data, ds)?;
                    // Strip the EOL that precedes endstream.
                    let mut e = found;
                    if e > ds && data.get(e - 1) == Some(&b'\n') {
                        e -= 1;
                    }
                    if e > ds && data.get(e - 1) == Some(&b'\r') {
                        e -= 1;
                    }
                    (e - ds, found + 9, true)
                }
            };
            let st = Stream {
                dict,
                data_start: ds as u64,
                data_len: dlen as u64,
                length_repaired: repaired,
            };
            let mut l3 = Lexer::new(data, after_es);
            match l3.next_tok() {
                Some((_, Tok::Keyword(k))) if k == b"endobj" => {
                    (Obj::Stream(Box::new(st)), l3.pos, false)
                }
                _ => (Obj::Stream(Box::new(st)), after_es, true),
            }
        }
        Some((_, Tok::Keyword(k))) if k == b"endobj" => (obj, lx.pos, false),
        _ => (obj, save, true),
    };
    Some(IndirectObj {
        num,
        gen_,
        obj,
        start: pos,
        end: end_pos,
        endobj_missing,
    })
}

/// First occurrence of `needle` at or after `from`.
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    let first = needle[0];
    let mut i = from;
    while i + needle.len() <= hay.len() {
        i += hay.get(i..)?.iter().position(|&b| b == first)?;
        if hay.get(i..i + needle.len()) == Some(needle) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Last occurrence of `needle` that starts before `before`.
pub fn rfind(hay: &[u8], needle: &[u8], before: usize) -> Option<usize> {
    let end = before.min(hay.len());
    if needle.len() > end {
        return None;
    }
    (0..=end - needle.len())
        .rev()
        .find(|&i| hay.get(i..i + needle.len()) == Some(needle))
}

/// Object id as reported, e.g. "12 0".
pub fn id_str(r: Ref) -> String {
    format!("{} {}", r.0, r.1)
}

/// Decodes a PDF text string (UTF-16BE/LE with BOM, UTF-8 with BOM, else PDFDocEncoding).
pub fn text_string(b: &[u8]) -> String {
    if b.len() >= 2 && b[0] == 0xFE && b[1] == 0xFF {
        let u: Vec<u16> = b[2..]
            .chunks(2)
            .map(|c| u16::from(c[0]) << 8 | u16::from(*c.get(1).unwrap_or(&0)))
            .collect();
        return String::from_utf16_lossy(&u);
    }
    if b.len() >= 2 && b[0] == 0xFF && b[1] == 0xFE {
        let u: Vec<u16> = b[2..]
            .chunks(2)
            .map(|c| u16::from(*c.get(1).unwrap_or(&0)) << 8 | u16::from(c[0]))
            .collect();
        return String::from_utf16_lossy(&u);
    }
    if b.len() >= 3 && b[0] == 0xEF && b[1] == 0xBB && b[2] == 0xBF {
        return String::from_utf8_lossy(&b[3..]).into_owned();
    }
    b.iter().map(|&c| pdfdoc_char(c)).collect()
}

fn pdfdoc_char(c: u8) -> char {
    const HIGH: [u32; 33] = [
        0x2022, 0x2020, 0x2021, 0x2026, 0x2014, 0x2013, 0x0192, 0x2044, 0x2039, 0x203A, 0x2212,
        0x2030, 0x201E, 0x201C, 0x201D, 0x2018, 0x2019, 0x201A, 0x2122, 0xFB01, 0xFB02, 0x0141,
        0x0152, 0x0160, 0x0178, 0x017D, 0x0131, 0x0142, 0x0153, 0x0161, 0x017E, 0xFFFD, 0x20AC,
    ];
    const LOW: [u32; 8] = [
        0x02D8, 0x02C7, 0x02C6, 0x02D9, 0x02DD, 0x02DB, 0x02DA, 0x02DC,
    ];
    let cp = match c {
        0x18..=0x1F => LOW[usize::from(c - 0x18)],
        0x80..=0xA0 => HIGH[usize::from(c - 0x80)],
        _ => u32::from(c),
    };
    char::from_u32(cp).unwrap_or('\u{FFFD}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_objects() {
        let mut p = Parser::new(
            b"<< /A 1 /B [2 0 R (x\\)y) <414> /N#20m] /C << /D true >> >>",
            0,
        );
        let o = p.object(0).unwrap();
        let d = o.as_dict().unwrap();
        assert_eq!(d.int(b"A"), Some(1));
        let b = d.get(b"B").unwrap().as_array().unwrap();
        assert_eq!(b[0], Obj::Ref(2, 0));
        assert_eq!(b[1], Obj::Str(b"x)y".to_vec()));
        assert_eq!(b[2], Obj::Str(b"A@".to_vec()));
        assert_eq!(b[3], Obj::Name(b"N m".to_vec()));
    }

    #[test]
    fn depth_limit() {
        let deep = "[".repeat(10_000);
        assert!(Parser::new(deep.as_bytes(), 0).object(0).is_none());
    }

    #[test]
    fn numbers() {
        assert_eq!(number(b"-.5"), Some(Tok::Real(-0.5)));
        assert_eq!(number(b"4."), Some(Tok::Real(4.0)));
        assert_eq!(number(b"+12"), Some(Tok::Int(12)));
        assert_eq!(number(b"--1"), None);
        assert_eq!(number(b"."), None);
        assert!(matches!(
            number(b"99999999999999999999999"),
            Some(Tok::Real(_))
        ));
    }

    #[test]
    fn stream_with_wrong_length_is_marked() {
        let d = b"1 0 obj\n<< /Length 99 >>\nstream\nabc\nendstream\nendobj\n";
        let ctx = ObjCtx {
            resolve_len: &|_| None,
            endstreams: None,
        };
        let o = parse_indirect(d, 0, &ctx).unwrap();
        let s = o.obj.as_stream().unwrap();
        assert!(s.length_repaired);
        assert_eq!(s.data_len, 3);
    }

    #[test]
    fn text_strings() {
        assert_eq!(text_string(b"\xFE\xFF\x00A\x00b"), "Ab");
        assert_eq!(text_string(b"a\x80"), "a\u{2022}");
    }
}
