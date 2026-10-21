//! PDF signatures: ByteRange coverage, CMS message digest and RSA PKCS#1 v1.5 signature maths for
//! adbe.pkcs7.detached / ETSI.CAdES.detached, signer subject, signing time, DocMDP.
//! Trust chain and revocation are NOT validated (v1 scope, see ADR 0001).
//!
//! CMS is walked with a minimal DER reader (no SET-ordering strictness); certificates are parsed
//! with `x509-cert` and keys with `rsa`.

use crate::doc::{Doc, Loc};
use crate::meta::iso_utc;
use crate::obj::{Dict, Lexer, Obj, Ref, Tok, text_string};
use der::Decode;
use sha2_legacy::Digest;
use std::collections::{HashMap, HashSet};

const OID_SIGNED_DATA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x07, 0x02];
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
const OID_SHA384: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
const OID_SHA512: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03];
const OID_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
const OID_RSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B];
const OID_RSA_SHA384: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C];
const OID_RSA_SHA512: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D];
const OID_MESSAGE_DIGEST: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x04];
const OID_SIGNING_TIME: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x05];

#[derive(Clone, Copy, Debug)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub raw: &'a [u8],
}

/// Reads one DER TLV (definite lengths only, ≤ 4 length bytes). Returns it and the rest.
pub fn tlv(d: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *d.first()?;
    if tag & 0x1F == 0x1F {
        return None; // high tag numbers are not used in CMS
    }
    let l0 = *d.get(1)?;
    let (len, hdr) = if l0 < 0x80 {
        (usize::from(l0), 2)
    } else {
        let n = usize::from(l0 & 0x7F);
        if n == 0 || n > 4 {
            return None;
        }
        let mut v = 0usize;
        for i in 0..n {
            v = (v << 8) | usize::from(*d.get(2 + i)?);
        }
        (v, 2 + n)
    };
    let end = hdr.checked_add(len)?;
    let raw = d.get(..end)?;
    Some((
        Tlv {
            tag,
            content: raw.get(hdr..)?,
            raw,
        },
        d.get(end..)?,
    ))
}

fn children(content: &[u8]) -> Vec<Tlv<'_>> {
    let mut out = Vec::new();
    let mut rest = content;
    while !rest.is_empty() && out.len() < 10_000 {
        match tlv(rest) {
            Some((t, r)) => {
                out.push(t);
                rest = r;
            }
            None => break,
        }
    }
    out
}

#[derive(Debug, Default, Clone)]
pub struct CmsCheck {
    pub digest_ok: Option<bool>,
    pub signature_ok: Option<bool>,
    pub signer_subject: Option<String>,
    pub signing_time: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum Hash {
    S256,
    S384,
    S512,
}

fn hash_of(oid: &[u8]) -> Option<Hash> {
    match oid {
        OID_SHA256 => Some(Hash::S256),
        OID_SHA384 => Some(Hash::S384),
        OID_SHA512 => Some(Hash::S512),
        _ => None,
    }
}

fn digest(h: Hash, parts: &[&[u8]]) -> Vec<u8> {
    match h {
        Hash::S256 => {
            let mut d = sha2_legacy::Sha256::new();
            for p in parts {
                d.update(p);
            }
            d.finalize().to_vec()
        }
        Hash::S384 => {
            let mut d = sha2_legacy::Sha384::new();
            for p in parts {
                d.update(p);
            }
            d.finalize().to_vec()
        }
        Hash::S512 => {
            let mut d = sha2_legacy::Sha512::new();
            for p in parts {
                d.update(p);
            }
            d.finalize().to_vec()
        }
    }
}

fn alg_oid<'a>(t: &Tlv<'a>) -> Option<&'a [u8]> {
    let c = children(t.content);
    let o = c.first()?;
    (o.tag == 0x06).then_some(o.content)
}

fn parse_time(t: &Tlv) -> Option<String> {
    let s = std::str::from_utf8(t.content).ok()?;
    let s = s.strip_suffix('Z')?;
    let (y, rest) = match t.tag {
        0x17 => {
            let yy: i64 = s.get(0..2)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, s.get(2..)?)
        }
        0x18 => (s.get(0..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    let f = |a: usize| -> Option<i64> { rest.get(a..a + 2)?.parse().ok() };
    let iso = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        f(0)?,
        f(2)?,
        f(4)?,
        f(6)?,
        f(8).unwrap_or(0)
    );
    // Normalise through the XMP parser (validates ranges).
    crate::meta::parse_xmp_instant(&iso).map(|i| iso_utc(i.secs))
}

/// Verifies a detached CMS SignedData over `signed` (the ByteRange bytes).
pub fn check_cms(cms: &[u8], signed: &[&[u8]]) -> CmsCheck {
    let mut out = CmsCheck::default();
    let r = check_cms_inner(cms, signed, &mut out);
    if r.is_none() && out.note.is_none() {
        out.note = Some("CMS structure could not be parsed".into());
    }
    out
}

fn check_cms_inner(cms: &[u8], signed: &[&[u8]], out: &mut CmsCheck) -> Option<()> {
    let (ci, _) = tlv(cms)?;
    let ci_c = children(ci.content);
    if ci_c.first()?.content != OID_SIGNED_DATA {
        out.note = Some("CMS content is not SignedData".into());
        return None;
    }
    let explicit = ci_c.get(1)?;
    let (sd, _) = tlv(explicit.content)?;
    let sdc = children(sd.content);
    // version, digestAlgorithms, encapContentInfo, [0] certs?, [1] crls?, signerInfos
    let mut certs: Vec<x509_cert::Certificate> = Vec::new();
    let mut signer_infos: Option<Tlv> = None;
    for t in sdc.iter().skip(3) {
        match t.tag {
            0xA0 => {
                for c in children(t.content) {
                    if c.tag == 0x30
                        && let Ok(cert) = x509_cert::Certificate::from_der(c.raw)
                    {
                        certs.push(cert);
                    }
                }
            }
            0x31 => signer_infos = Some(*t),
            _ => {}
        }
    }
    let si_set = signer_infos?;
    let si = *children(si_set.content).first()?;
    let sic = children(si.content);
    // version, sid, digestAlgorithm, [0] signedAttrs?, signatureAlgorithm, signature
    let sid = *sic.get(1)?;
    let dig_alg = alg_oid(sic.get(2)?)?;
    let mut idx = 3;
    let signed_attrs = match sic.get(3) {
        Some(t) if t.tag == 0xA0 => {
            idx = 4;
            Some(*t)
        }
        _ => None,
    };
    let sig_alg = alg_oid(sic.get(idx)?)?;
    let sig_val = sic.get(idx + 1)?;
    if sig_val.tag != 0x04 {
        return None;
    }
    // Signer certificate.
    let cert = find_signer(&certs, &sid);
    if let Some(c) = cert {
        out.signer_subject = Some(c.tbs_certificate.subject.to_string());
    }
    let Some(h) = hash_of(dig_alg) else {
        out.note = Some("digest algorithm not supported (not checked)".into());
        return Some(());
    };
    let content_digest = digest(h, signed);
    let to_verify: Vec<u8>;
    match signed_attrs {
        Some(sa) => {
            let mut md: Option<&[u8]> = None;
            for attr in children(sa.content) {
                let ac = children(attr.content);
                let (Some(oid), Some(vals)) = (ac.first(), ac.get(1)) else {
                    continue;
                };
                let first_val = children(vals.content).first().copied();
                if oid.content == OID_MESSAGE_DIGEST {
                    md = first_val.filter(|v| v.tag == 0x04).map(|v| v.content);
                } else if oid.content == OID_SIGNING_TIME {
                    out.signing_time = first_val.and_then(|v| parse_time(&v));
                }
            }
            out.digest_ok = Some(md.is_some_and(|m| m == content_digest.as_slice()));
            // Signed attributes are signed as an explicit SET OF (tag 0x31).
            let mut v = sa.raw.to_vec();
            if let Some(first) = v.first_mut() {
                *first = 0x31;
            }
            to_verify = v;
        }
        None => {
            // No signed attributes: the signature is directly over the content digest.
            out.digest_ok = None;
            to_verify = Vec::new();
        }
    }
    let rsa_hash = match sig_alg {
        OID_RSA => Some(h),
        OID_RSA_SHA256 => Some(Hash::S256),
        OID_RSA_SHA384 => Some(Hash::S384),
        OID_RSA_SHA512 => Some(Hash::S512),
        _ => None,
    };
    let (Some(sh), Some(cert)) = (rsa_hash, cert) else {
        out.note = Some(if cert.is_none() {
            "signer certificate not found in CMS (signature not checked)".into()
        } else {
            "signature algorithm not supported (not checked)".into()
        });
        return Some(());
    };
    use der::Encode;
    use rsa::pkcs8::DecodePublicKey;
    let spki = cert.tbs_certificate.subject_public_key_info.to_der().ok()?;
    let Ok(key) = rsa::RsaPublicKey::from_public_key_der(&spki) else {
        out.note = Some("signer key is not RSA (signature not checked)".into());
        return Some(());
    };
    let hashed = if signed_attrs.is_some() {
        digest(sh, &[&to_verify])
    } else {
        if sh != h {
            return Some(());
        }
        content_digest
    };
    let scheme = match sh {
        Hash::S256 => rsa::Pkcs1v15Sign::new::<sha2_legacy::Sha256>(),
        Hash::S384 => rsa::Pkcs1v15Sign::new::<sha2_legacy::Sha384>(),
        Hash::S512 => rsa::Pkcs1v15Sign::new::<sha2_legacy::Sha512>(),
    };
    out.signature_ok = Some(key.verify(scheme, &hashed, sig_val.content).is_ok());
    Some(())
}

fn find_signer<'c>(
    certs: &'c [x509_cert::Certificate],
    sid: &Tlv,
) -> Option<&'c x509_cert::Certificate> {
    if sid.tag == 0x30 {
        let c = children(sid.content);
        let (issuer, serial) = (c.first()?, c.get(1)?);
        let name = x509_cert::name::Name::from_der(issuer.raw).ok();
        if let Some(found) = certs.iter().find(|cert| {
            name.as_ref() == Some(&cert.tbs_certificate.issuer)
                && cert.tbs_certificate.serial_number.as_bytes() == serial.content
        }) {
            return Some(found);
        }
    } else if sid.tag == 0x80 {
        let ski_oid = const_oid::ObjectIdentifier::new_unwrap("2.5.29.14");
        if let Some(found) = certs.iter().find(|cert| {
            cert.tbs_certificate
                .extensions
                .as_ref()
                .is_some_and(|exts| {
                    exts.iter().any(|e| {
                        e.extn_id == ski_oid
                            && tlv(e.extn_value.as_bytes())
                                .is_some_and(|(t, _)| t.content == sid.content)
                    })
                })
        }) {
            return Some(found);
        }
    }
    if certs.len() == 1 {
        certs.first()
    } else {
        None
    }
}

/// Byte span of the /Contents string of the dictionary of the object at `off`.
pub fn contents_span(data: &[u8], off: usize) -> Option<(usize, usize)> {
    let (_, _, after) = crate::obj::read_obj_header(data, off)?;
    let mut lx = Lexer::new(data, after);
    let mut depth = 0i32;
    let mut pending_contents = false;
    for _ in 0..100_000 {
        let (start, t) = lx.next_tok()?;
        match t {
            Tok::DictOpen | Tok::ArrOpen => {
                depth += 1;
                pending_contents = false;
            }
            Tok::DictClose | Tok::ArrClose => {
                depth -= 1;
                if depth <= 0 {
                    return None;
                }
            }
            Tok::Name(n) if depth == 1 => pending_contents = n == b"Contents",
            Tok::Str(_) if depth == 1 && pending_contents => return Some((start, lx.pos)),
            Tok::Keyword(k) if k == b"endobj" || k == b"stream" => return None,
            _ => pending_contents = false,
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct SigFound {
    pub obj: Ref,
    pub first_rev: usize,
    pub dict: Dict,
    pub loc: Loc,
    pub field: Option<String>,
    pub doc_timestamp: bool,
}

/// Signature-value dictionaries in any revision (first version of each object).
pub fn find_signatures(doc: &Doc) -> Vec<SigFound> {
    let mut seen: HashSet<Ref> = HashSet::new();
    let mut out = Vec::new();
    for rev in 1..=doc.revs.len() {
        for (num, _) in doc.section_entries(rev) {
            let Some(g) = doc.gen_of(rev, num) else {
                continue;
            };
            let r = (num, g);
            if seen.contains(&r) {
                continue;
            }
            let Some((o, loc)) = doc.resolve_loc(rev, r) else {
                continue;
            };
            let Obj::Dict(d) = o else { continue };
            let is_sig = d.get(b"ByteRange").and_then(Obj::as_array).is_some()
                && matches!(d.get(b"Contents"), Some(Obj::Str(_)))
                && (d.type_is(b"Sig") || d.type_is(b"DocTimeStamp") || !d.has(b"Type"));
            if is_sig {
                seen.insert(r);
                out.push(SigFound {
                    obj: r,
                    first_rev: rev,
                    doc_timestamp: d.type_is(b"DocTimeStamp"),
                    dict: d,
                    loc,
                    field: None,
                });
            }
        }
    }
    // Field names from the AcroForm of every revision (latest name wins).
    let mut names: HashMap<Ref, String> = HashMap::new();
    for rev in 1..=doc.revs.len() {
        for (v, name) in sig_fields(doc, rev) {
            names.insert(v, name);
        }
    }
    for s in out.iter_mut() {
        s.field = names.get(&s.obj).cloned();
    }
    out
}

/// (signature value ref, full field name) of signature fields in revision `rev`.
pub fn sig_fields(doc: &Doc, rev: usize) -> Vec<(Ref, String)> {
    let mut out = Vec::new();
    let Some(cat) = doc.catalog(rev) else {
        return out;
    };
    let Some(af) = cat.get(b"AcroForm").and_then(|a| doc.get_dict(rev, a)) else {
        return out;
    };
    let Some(fields) = af.get(b"Fields").and_then(|f| doc.deref(rev, f)) else {
        return out;
    };
    let mut stack: Vec<(Obj, String, Option<Vec<u8>>, usize)> = fields
        .as_array()
        .unwrap_or(&[])
        .iter()
        .rev()
        .map(|f| (f.clone(), String::new(), None, 0))
        .collect();
    let mut visited = HashSet::new();
    while let Some((f, parent, ft, depth)) = stack.pop() {
        if depth > 32 || out.len() > 10_000 {
            continue;
        }
        if let Some(r) = f.as_ref()
            && !visited.insert(r)
        {
            continue;
        }
        let Some(d) = doc.get_dict(rev, &f) else {
            continue;
        };
        let t = d
            .get(b"T")
            .and_then(|t| doc.deref(rev, t))
            .and_then(|t| t.as_str().map(text_string));
        let name = match (parent.is_empty(), t) {
            (_, None) => parent.clone(),
            (true, Some(t)) => t,
            (false, Some(t)) => format!("{}.{}", parent, t),
        };
        let ft = d.name(b"FT").map(<[u8]>::to_vec).or(ft);
        if ft.as_deref() == Some(b"Sig")
            && let Some(Obj::Ref(n, g)) = d.get(b"V")
        {
            out.push(((*n, *g), name.clone()));
        }
        if let Some(Obj::Array(kids)) = d.get(b"Kids").and_then(|k| doc.deref(rev, k)) {
            for k in kids.into_iter().rev() {
                stack.push((k, name.clone(), ft.clone(), depth + 1));
            }
        }
    }
    out
}

/// DocMDP permission of a signature: from the catalog /Perms /DocMDP of any revision or the
/// signature's own /Reference transform.
pub fn docmdp_level(doc: &Doc, s: &SigFound) -> Option<u8> {
    let from_refs = |d: &Dict, rev: usize| -> Option<u8> {
        let refs = doc.deref(rev, d.get(b"Reference")?)?;
        for r in refs.as_array()? {
            let rd = doc.get_dict(rev, r)?;
            if rd.name(b"TransformMethod") == Some(b"DocMDP") {
                let p = rd
                    .get(b"TransformParams")
                    .and_then(|t| doc.get_dict(rev, t))
                    .and_then(|t| t.int(b"P"))
                    .unwrap_or(2);
                return Some(p.clamp(1, 3) as u8);
            }
        }
        None
    };
    let mut certified = false;
    for rev in s.first_rev..=doc.revs.len() {
        if let Some(cat) = doc.catalog(rev)
            && let Some(perms) = cat.get(b"Perms").and_then(|p| doc.get_dict(rev, p))
            && perms.get(b"DocMDP").and_then(Obj::as_ref) == Some(s.obj)
        {
            certified = true;
            break;
        }
    }
    let lvl = from_refs(&s.dict, s.first_rev);
    if certified {
        Some(lvl.unwrap_or(2))
    } else {
        lvl
    }
}
