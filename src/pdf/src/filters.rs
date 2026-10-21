//! Stream filters (decode only) with hard output limits. Image codecs (DCT, JPX, CCITT, JBIG2)
//! are not decoded: their raw bytes are what the core extracts.

use crate::obj::{Dict, Obj};

/// Largest decoded size of a single stream.
pub const MAX_DECODED: usize = 128 * 1024 * 1024;
const MAX_FILTERS: usize = 8;

#[derive(Debug, PartialEq)]
pub enum DecodeError {
    /// A filter we do not decode (image codecs, Crypt, unknown names).
    Unsupported(String),
    /// Corrupt data or a limit was reached.
    Corrupt(String),
}

/// Filter names of a stream dictionary.
pub fn filter_names(dict: &Dict) -> Vec<String> {
    match dict.get(b"Filter") {
        Some(Obj::Name(n)) => vec![String::from_utf8_lossy(n).into_owned()],
        Some(Obj::Array(a)) => a
            .iter()
            .filter_map(Obj::as_name)
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect(),
        _ => Vec::new(),
    }
}

fn parms_at(dict: &Dict, i: usize) -> Option<&Dict> {
    match dict.get(b"DecodeParms").or_else(|| dict.get(b"DP")) {
        Some(Obj::Dict(d)) if i == 0 => Some(d),
        Some(Obj::Array(a)) => a.get(i).and_then(Obj::as_dict),
        _ => None,
    }
}

/// Decodes `raw` through the stream's filter chain. `partial_ok`: corrupt Flate data yields what
/// was decoded so far plus an error message the caller turns into a warning.
pub fn decode(dict: &Dict, raw: &[u8]) -> Result<(Vec<u8>, Option<String>), DecodeError> {
    let names = filter_names(dict);
    if names.len() > MAX_FILTERS {
        return Err(DecodeError::Corrupt("too many filters".into()));
    }
    let mut data = raw.to_vec();
    let mut note = None;
    for (i, n) in names.iter().enumerate() {
        let parms = parms_at(dict, i);
        data = match n.as_str() {
            "FlateDecode" | "Fl" => {
                let (d, e) = flate(&data)?;
                if e.is_some() {
                    note = e;
                }
                predictor(d, parms)?
            }
            "LZWDecode" | "LZW" => {
                let early = parms.and_then(|p| p.int(b"EarlyChange")).unwrap_or(1) != 0;
                predictor(lzw(&data, early)?, parms)?
            }
            "ASCIIHexDecode" | "AHx" => ascii_hex(&data),
            "ASCII85Decode" | "A85" => ascii85(&data)?,
            "RunLengthDecode" | "RL" => run_length(&data)?,
            other => return Err(DecodeError::Unsupported(other.to_string())),
        };
        if data.len() > MAX_DECODED {
            return Err(DecodeError::Corrupt("decoded stream too large".into()));
        }
    }
    Ok((data, note))
}

fn flate(data: &[u8]) -> Result<(Vec<u8>, Option<String>), DecodeError> {
    use miniz_oxide::inflate::{decompress_to_vec_with_limit, decompress_to_vec_zlib_with_limit};
    match decompress_to_vec_zlib_with_limit(data, MAX_DECODED) {
        Ok(v) => Ok((v, None)),
        Err(e) => {
            if let Ok(v) = decompress_to_vec_with_limit(data, MAX_DECODED) {
                return Ok((v, Some("FlateDecode stream without zlib header".into())));
            }
            if e.output.is_empty() {
                Err(DecodeError::Corrupt(format!(
                    "FlateDecode failed: {:?}",
                    e.status
                )))
            } else {
                Ok((
                    e.output,
                    Some(format!(
                        "FlateDecode data corrupt ({:?}); partial data used",
                        e.status
                    )),
                ))
            }
        }
    }
}

fn predictor(data: Vec<u8>, parms: Option<&Dict>) -> Result<Vec<u8>, DecodeError> {
    let Some(p) = parms else { return Ok(data) };
    let pred = p.int(b"Predictor").unwrap_or(1);
    if pred <= 1 {
        return Ok(data);
    }
    let colors = p.int(b"Colors").unwrap_or(1).clamp(1, 32) as usize;
    let bpc = p.int(b"BitsPerComponent").unwrap_or(8).clamp(1, 16) as usize;
    let columns = p.int(b"Columns").unwrap_or(1).clamp(1, 1 << 20) as usize;
    let bpp = (colors * bpc).div_ceil(8).max(1);
    let row = (colors * bpc * columns).div_ceil(8);
    if pred == 2 {
        if bpc != 8 {
            return Ok(data);
        }
        let mut out = data;
        for r in out.chunks_mut(row.max(1)) {
            for i in bpp..r.len() {
                r[i] = r[i].wrapping_add(r[i - bpp]);
            }
        }
        return Ok(out);
    }
    if !(10..=15).contains(&pred) {
        return Err(DecodeError::Corrupt(format!("unknown predictor {}", pred)));
    }
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        let Some((&ft, src)) = chunk.split_first() else {
            break;
        };
        let mut cur = vec![0u8; row];
        for i in 0..row.min(src.len()) {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let x = src[i];
            cur[i] = match ft {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => return Err(DecodeError::Corrupt("bad PNG filter type".into())),
            };
        }
        out.extend_from_slice(&cur[..row.min(src.len())]);
        prev = cur;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let pa = (p - i16::from(a)).abs();
    let pb = (p - i16::from(b)).abs();
    let pc = (p - i16::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut hi: Option<u8> = None;
    for &c in data {
        if c == b'>' {
            break;
        }
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => continue,
        };
        match hi.take() {
            Some(h) => out.push(h << 4 | v),
            None => hi = Some(v),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn ascii85(data: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(data.len());
    let mut group = [0u8; 5];
    let mut n = 0usize;
    let body = data.strip_prefix(b"<~").unwrap_or(data);
    for &c in body {
        if c == b'~' {
            break;
        }
        if crate::obj::is_ws(c) {
            continue;
        }
        if c == b'z' && n == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&c) {
            return Err(DecodeError::Corrupt("bad ASCII85 character".into()));
        }
        group[n] = c - b'!';
        n += 1;
        if n == 5 {
            let v = group.iter().fold(0u64, |acc, &d| acc * 85 + u64::from(d));
            if v > u64::from(u32::MAX) {
                return Err(DecodeError::Corrupt("ASCII85 overflow".into()));
            }
            out.extend_from_slice(&(v as u32).to_be_bytes());
            n = 0;
        }
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u64, |acc, &d| acc * 85 + u64::from(d));
        let b = (v.min(u64::from(u32::MAX)) as u32).to_be_bytes();
        out.extend_from_slice(&b[..n - 1]);
    }
    Ok(out)
}

fn run_length(data: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&l) = data.get(i) {
        i += 1;
        if l == 128 {
            break;
        }
        if l < 128 {
            let n = usize::from(l) + 1;
            let s = data.get(i..i + n).unwrap_or(data.get(i..).unwrap_or(&[]));
            out.extend_from_slice(s);
            i += n;
        } else {
            let n = 257 - usize::from(l);
            if let Some(&b) = data.get(i) {
                out.extend(std::iter::repeat_n(b, n));
            }
            i += 1;
        }
        if out.len() > MAX_DECODED {
            return Err(DecodeError::Corrupt("RunLength output too large".into()));
        }
    }
    Ok(out)
}

fn lzw(data: &[u8], early: bool) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    let mut dict: Vec<Vec<u8>> = (0..=255u16).map(|b| vec![b as u8]).collect();
    dict.push(Vec::new()); // 256 clear
    dict.push(Vec::new()); // 257 EOD
    let mut width = 9u32;
    let mut bitbuf: u32 = 0;
    let mut nbits = 0u32;
    let mut prev: Option<Vec<u8>> = None;
    for &byte in data {
        bitbuf = (bitbuf << 8) | u32::from(byte);
        nbits += 8;
        while nbits >= width {
            let code = ((bitbuf >> (nbits - width)) & ((1 << width) - 1)) as usize;
            nbits -= width;
            bitbuf &= (1u32 << nbits).wrapping_sub(1);
            if code == 256 {
                dict.truncate(258);
                width = 9;
                prev = None;
                continue;
            }
            if code == 257 {
                return Ok(out);
            }
            let entry = if let Some(e) = dict.get(code) {
                e.clone()
            } else if code == dict.len()
                && let Some(p) = &prev
            {
                let mut e = p.clone();
                e.push(*p.first().unwrap_or(&0));
                e
            } else {
                return Err(DecodeError::Corrupt("bad LZW code".into()));
            };
            out.extend_from_slice(&entry);
            if out.len() > MAX_DECODED {
                return Err(DecodeError::Corrupt("LZW output too large".into()));
            }
            if let Some(mut p) = prev.take()
                && dict.len() < 4096
            {
                p.push(*entry.first().unwrap_or(&0));
                dict.push(p);
            }
            prev = Some(entry);
            let lim = dict.len() + usize::from(early);
            width = if lim >= 2048 {
                12
            } else if lim >= 1024 {
                11
            } else if lim >= 512 {
                10
            } else {
                9
            };
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obj::Parser;

    fn dict(s: &str) -> Dict {
        Parser::new(s.as_bytes(), 0)
            .object(0)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone()
    }

    #[test]
    fn flate_and_png_up() {
        let rows = [[2u8, 1, 2, 3], [2, 1, 1, 1]];
        let raw: Vec<u8> = rows.iter().flatten().copied().collect();
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        let d = dict("<< /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 3 >> >>");
        let (out, note) = decode(&d, &z).unwrap();
        assert_eq!(out, vec![1, 2, 3, 2, 3, 4]);
        assert!(note.is_none());
    }

    #[test]
    fn ascii_filters() {
        let d = dict("<< /Filter [/ASCIIHexDecode] >>");
        assert_eq!(decode(&d, b"48 65 6C6C6F>").unwrap().0, b"Hello");
        let d = dict("<< /Filter /ASCII85Decode >>");
        assert_eq!(decode(&d, b"87cURD]i,\"Ebo7~>").unwrap().0, b"Hello World");
    }

    #[test]
    fn garbage_never_panics() {
        let d = dict("<< /Filter [/FlateDecode /LZWDecode /RunLengthDecode] >>");
        for seed in 0..200u32 {
            let g: Vec<u8> = (0..64u32)
                .map(|i| (i.wrapping_mul(seed * 31 + 7) >> 3) as u8)
                .collect();
            let _ = decode(&d, &g);
            let _ = lzw(&g, true);
            let _ = run_length(&g);
            let _ = ascii85(&g);
        }
    }
}
