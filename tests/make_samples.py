#!/usr/bin/env python3
"""Generates the synthetic sample PDFs in examples/sample/evidence/ (deterministic: fixed dates,
fixed /ID, seeded keys and images). Every file is a SYNTHETIC TEST SAMPLE: the producer strings of
real products are written only so that the fingerprint matching can be exercised.

  python tests/make_samples.py [out_dir]          default: examples/sample/evidence

Needs (test-only, never shipped): reportlab, pikepdf, pillow, cryptography, asn1crypto, blake3.
Also rewrites examples/sample/expected_hashes.csv (file,size,md5,sha1,sha256,sha512,sha3-256,blake3).
"""

from __future__ import annotations

import datetime as dt
import hashlib
import io
import math
import random
import re
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))

import fake_tsa  # noqa: E402  (test PKI: names, password)
import pikepdf  # noqa: E402
from asn1crypto import algos, cms  # noqa: E402
from asn1crypto import x509 as ax509  # noqa: E402
from cryptography import x509 as cx  # noqa: E402
from cryptography.hazmat.primitives import hashes, serialization  # noqa: E402
from cryptography.hazmat.primitives.asymmetric import padding, rsa  # noqa: E402
from cryptography.hazmat.primitives.serialization import pkcs12  # noqa: E402
from PIL import Image, ImageDraw  # noqa: E402
from reportlab.lib.utils import ImageReader  # noqa: E402
from reportlab.pdfgen import canvas  # noqa: E402

UTC = dt.timezone.utc
COT = dt.timezone(dt.timedelta(hours=-5))  # Bogotá


def pdf_date(t: dt.datetime) -> str:
    off = t.utcoffset() or dt.timedelta(0)
    mins = int(off.total_seconds() // 60)
    sign = "+" if mins >= 0 else "-"
    mins = abs(mins)
    return "D:" + t.strftime("%Y%m%d%H%M%S") + f"{sign}{mins // 60:02d}'{mins % 60:02d}'"


def pdf_str(s: str) -> bytes:
    return b"(" + s.encode("latin-1").replace(b"\\", b"\\\\").replace(b"(", b"\\(").replace(b")", b"\\)") + b")"


# ------------------------------------------------------------------ base documents (ReportLab)


def reportlab_doc(draw, when: dt.datetime, title: str, pages: int = 1) -> bytes:
    buf = io.BytesIO()
    c = canvas.Canvas(buf, invariant=1, pagesize=(612, 792))
    c.setDateFormatter(lambda *a: pdf_date(when))
    c.setTitle(title)
    c.setAuthor("Rakvor test generator")
    c.setCreator("Rakvor make_samples.py")
    for i in range(pages):
        draw(c, i)
        c.showPage()
    c.save()
    return buf.getvalue()


def invoice_page(amount: str):
    def draw(c, _i):
        c.setFont("Helvetica-Bold", 16)
        c.drawString(72, 720, "SYNTHETIC TEST SAMPLE - INVOICE 2026-031")
        c.setFont("Helvetica", 12)
        c.drawString(72, 690, "Customer: Example Ltda.")
        c.drawString(72, 670, f"Amount due: {amount}")

    return draw


# ------------------------------------------------------------------ incremental updates


class Incremental:
    """Appends one incremental update (new/replaced objects + xref table + trailer) to a PDF."""

    def __init__(self, base: bytes):
        self.base = base if base.endswith(b"\n") else base + b"\n"
        with pikepdf.open(io.BytesIO(base)) as p:
            self.size = int(p.trailer.Size)
            self.root = p.trailer.Root.objgen
            self.info = p.trailer.Info.objgen if "/Info" in p.trailer else None
            self.id0 = bytes(p.trailer.ID[0]) if "/ID" in p.trailer else hashlib.md5(base).digest()
        self.prev = int(re.findall(rb"startxref\s+(\d+)", base)[-1])
        self.objs: dict[int, bytes] = {}

    def new_num(self) -> int:
        n = self.size
        self.size += 1
        return n

    def put(self, num: int, body: bytes) -> None:
        self.objs[num] = body

    def stream(self, num: int, data: bytes, extra: bytes = b"") -> None:
        self.put(num, b"<< /Length %d %s>>\nstream\n" % (len(data), extra) + data + b"\nendstream")

    def build(self, id1_seed: bytes) -> tuple[bytes, dict[int, int]]:
        out = bytearray(self.base)
        offsets = {}
        for num in sorted(self.objs):
            offsets[num] = len(out)
            out += b"%d 0 obj\n" % num + self.objs[num] + b"\nendobj\n"
        xref = len(out)
        out += b"xref\n"
        nums = sorted(self.objs)
        groups: list[list[int]] = []
        for n in nums:
            if groups and n == groups[-1][-1] + 1:
                groups[-1].append(n)
            else:
                groups.append([n])
        for g in groups:
            out += b"%d %d\n" % (g[0], len(g))
            for n in g:
                out += b"%010d 00000 n\r\n" % offsets[n]
        id1 = hashlib.md5(id1_seed).digest()
        trailer = b"<< /Size %d /Root %d %d R " % (self.size, *self.root)
        if self.info:
            trailer += b"/Info %d %d R " % self.info
        trailer += b"/Prev %d /ID [<%s> <%s>] >>" % (self.prev, self.id0.hex().encode(), id1.hex().encode())
        out += b"trailer\n" + trailer + b"\nstartxref\n%d\n%%%%EOF\n" % xref
        return bytes(out), offsets


def info_body(base: bytes, **changes: str) -> bytes:
    with pikepdf.open(io.BytesIO(base)) as p:
        info = {str(k)[1:]: str(v) for k, v in sorted(p.trailer.Info.items())}
    info.update(changes)
    return b"<< " + b" ".join(b"/" + k.encode() + b" " + pdf_str(v) for k, v in info.items()) + b" >>"


def u(v) -> bytes:
    """PDF syntax of a pikepdf value (pikepdf turns scalars into Python values)."""
    if isinstance(v, bool):
        return b"true" if v else b"false"
    if isinstance(v, (int, float)):
        return str(v).encode()
    return v.unparse()


class PageParts:
    """A page of an existing PDF, split so an update can rewrite its dictionary."""

    def __init__(self, base: bytes, index: int = 0):
        with pikepdf.open(io.BytesIO(base)) as p:
            page = p.pages[index].obj
            self.objgen = page.objgen
            keys = [k for k in sorted(page.keys()) if k not in ("/Contents", "/Resources", "/Annots")]
            self.rest = b" ".join(k.encode() + b" " + u(page[k]) for k in keys)
            res = page.Resources
            self.res_other = {k: u(res[k]) for k in sorted(res.keys()) if k != "/Font"}
            self.fonts = {}
            self.font_names = {}
            if "/Font" in res:
                for k, v in sorted(res.Font.items()):
                    self.fonts[k] = u(v)
                    self.font_names[str(v.BaseFont)] = k
            self.contents = page.Contents.objgen

    def resources(self, extra_fonts: dict[str, int] | None = None) -> bytes:
        fonts = dict(self.fonts)
        for k, num in (extra_fonts or {}).items():
            fonts[k] = b"%d 0 R" % num
        parts = [k.encode() + b" " + v for k, v in self.res_other.items()]
        if fonts:
            parts.append(b"/Font << " + b" ".join(k.encode() + b" " + v for k, v in fonts.items()) + b" >>")
        return b"<< " + b" ".join(parts) + b" >>"

    def dict(self, contents: bytes, extra_fonts: dict[str, int] | None = None, annots: bytes = b"") -> bytes:
        a = b" /Annots " + annots if annots else b""
        return b"<< " + self.rest + b" /Resources " + self.resources(extra_fonts) + b" /Contents " + contents + a + b" >>"


def invoice_text(fonts: dict[str, str], amount: str) -> bytes:
    b, r = fonts["/Helvetica-Bold"], fonts["/Helvetica"]
    return (
        f"BT {b} 16 Tf 72 720 Td (SYNTHETIC TEST SAMPLE - INVOICE 2026-031) Tj ET\n"
        f"BT {r} 12 Tf 72 690 Td (Customer: Example Ltda.) Tj ET\n"
        f"BT {r} 12 Tf 72 670 Td (Amount due: {amount}) Tj ET\n"
    ).encode()


# ------------------------------------------------------------------ deterministic test PKI


def _is_probable_prime(n: int, rng: random.Random) -> bool:
    if n < 4:
        return n in (2, 3)
    for p in (2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37):
        if n % p == 0:
            return n == p
    d, r = n - 1, 0
    while d % 2 == 0:
        d //= 2
        r += 1
    for _ in range(40):
        a = rng.randrange(2, n - 2)
        x = pow(a, d, n)
        if x in (1, n - 1):
            continue
        for _ in range(r - 1):
            x = pow(x, 2, n)
            if x == n - 1:
                break
        else:
            return False
    return True


def seeded_rsa(seed: str, bits: int = 2048) -> rsa.RSAPrivateKey:
    rng = random.Random(seed)
    e = 65537

    def prime() -> int:
        while True:
            n = rng.getrandbits(bits // 2) | (1 << (bits // 2 - 1)) | (1 << (bits // 2 - 2)) | 1
            if math.gcd(e, n - 1) == 1 and _is_probable_prime(n, rng):
                return n

    p, q = prime(), prime()
    n, lam = p * q, math.lcm(p - 1, q - 1)
    d = pow(e, -1, lam)
    pub = rsa.RSAPublicNumbers(e, n)
    return rsa.RSAPrivateNumbers(p, q, d, d % (p - 1), d % (q - 1), pow(q, -1, p), pub).private_key()


def test_pki(out: Path) -> Path:
    """Same layout as tests/fake_tsa.py (CA + examiner.p12, password "test"), but with seeded keys
    and fixed validity so the signed sample is reproducible."""
    nb, na = dt.datetime(2026, 1, 1, tzinfo=UTC), dt.datetime(2036, 1, 1, tzinfo=UTC)
    ca_key, ex_key = seeded_rsa("rakvor-test-ca"), seeded_rsa("rakvor-test-examiner")
    ca_name = fake_tsa._name("Rakvor TEST CA")

    def cert(subject, key, issuer_key, serial, ca):
        return (
            cx.CertificateBuilder()
            .subject_name(subject)
            .issuer_name(ca_name)
            .public_key(key.public_key())
            .serial_number(serial)
            .not_valid_before(nb)
            .not_valid_after(na)
            .add_extension(cx.BasicConstraints(ca=ca, path_length=None), critical=True)
            .sign(issuer_key, hashes.SHA256())
        )

    ca = cert(ca_name, ca_key, ca_key, 1001, True)
    ex = cert(fake_tsa._name("Ana Ruiz (TEST examiner)"), ex_key, ca_key, 1002, False)
    out.mkdir(parents=True, exist_ok=True)
    (out / "ca.pem").write_bytes(ca.public_bytes(serialization.Encoding.PEM))
    p12 = out / "examiner.p12"
    p12.write_bytes(
        pkcs12.serialize_key_and_certificates(
            b"examiner", ex_key, ex, [ca], serialization.BestAvailableEncryption(fake_tsa.PASSWORD)
        )
    )
    return p12


def cms_detached(p12: Path, data: bytes, when: dt.datetime) -> bytes:
    key, cert, extra = pkcs12.load_key_and_certificates(p12.read_bytes(), fake_tsa.PASSWORD)
    acert = ax509.Certificate.load(cert.public_bytes(serialization.Encoding.DER))
    chain = [ax509.Certificate.load(c.public_bytes(serialization.Encoding.DER)) for c in extra]
    attrs = cms.CMSAttributes(
        [
            cms.CMSAttribute({"type": "content_type", "values": ["data"]}),
            cms.CMSAttribute({"type": "signing_time", "values": [cms.Time({"utc_time": when})]}),
            cms.CMSAttribute({"type": "message_digest", "values": [hashlib.sha256(data).digest()]}),
        ]
    )
    sig = key.sign(attrs.dump(), padding.PKCS1v15(), hashes.SHA256())
    si = cms.SignerInfo(
        {
            "version": "v1",
            "sid": cms.SignerIdentifier(
                {
                    "issuer_and_serial_number": cms.IssuerAndSerialNumber(
                        {"issuer": acert.issuer, "serial_number": acert.serial_number}
                    )
                }
            ),
            "digest_algorithm": algos.DigestAlgorithm({"algorithm": "sha256"}),
            "signed_attrs": attrs,
            "signature_algorithm": algos.SignedDigestAlgorithm({"algorithm": "rsassa_pkcs1v15"}),
            "signature": sig,
        }
    )
    sd = cms.SignedData(
        {
            "version": "v1",
            "digest_algorithms": [algos.DigestAlgorithm({"algorithm": "sha256"})],
            "encap_content_info": {"content_type": "data"},
            "certificates": [acert, *chain],
            "signer_infos": [si],
        }
    )
    return cms.ContentInfo({"content_type": "signed_data", "content": sd}).dump()


# ------------------------------------------------------------------ the samples


def original() -> bytes:
    when = dt.datetime(2026, 2, 16, 10, 30, 0, tzinfo=COT)
    return reportlab_doc(invoice_page("EUR 1,250.00"), when, "SYNTHETIC TEST SAMPLE - original")


def edited_incremental() -> bytes:
    created = dt.datetime(2026, 3, 2, 9, 15, 0, tzinfo=COT)
    base = reportlab_doc(invoice_page("EUR 1,250.00"), created, "SYNTHETIC TEST SAMPLE - edited")
    editor = "Foxit PDF Editor Version 13.0.0"
    # Revision 2: the amount on page 1 is changed (new content stream), Info updated.
    up = Incremental(base)
    pp = PageParts(base)
    c2 = up.new_num()
    up.stream(c2, invoice_text(pp.font_names, "EUR 7,250.00"))
    up.put(pp.objgen[0], pp.dict(b"%d 0 R" % c2))
    up.put(up.info[0], info_body(base, ModDate=pdf_date(created + dt.timedelta(days=2, hours=3)), Producer=editor))
    rev2, _ = up.build(b"edited-incremental-rev2")
    # Revision 3: a new font (Courier-Bold) and a "PAID" stamp; ModDate 9 days after CreationDate.
    up = Incremental(rev2)
    font = up.new_num()
    up.put(font, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier-Bold /Encoding /WinAnsiEncoding >>")
    c3 = up.new_num()
    up.stream(c3, b"q 0.8 0 0 rg BT /FPAID 28 Tf 380 664 Td (PAID) Tj ET Q\n")
    up.put(pp.objgen[0], pp.dict(b"[%d 0 R %d 0 R]" % (c2, c3), {"/FPAID": font}))
    up.put(up.info[0], info_body(rev2, ModDate=pdf_date(created + dt.timedelta(days=9))))
    rev3, _ = up.build(b"edited-incremental-rev3")
    return rev3


def signed_then_modified(pki: Path) -> bytes:
    created = dt.datetime(2026, 5, 4, 8, 0, 0, tzinfo=COT)
    signed_at = dt.datetime(2026, 5, 4, 13, 20, 0, tzinfo=UTC)
    base = reportlab_doc(invoice_page("EUR 3,400.00"), created, "SYNTHETIC TEST SAMPLE - signed")
    with pikepdf.open(io.BytesIO(base)) as p:
        root = p.Root
        cat_rest = b" ".join(k.encode() + b" " + u(root[k]) for k in sorted(root.keys()))
    pp = PageParts(base)
    pg = pp.objgen
    # Revision 2: certification signature (DocMDP P=2), invisible widget.
    up = Incremental(base)
    sig, field, form = up.new_num(), up.new_num(), up.new_num()
    placeholder = b"0" * 8192
    br_ph = b"[0 0000000000 0000000000 0000000000]"
    up.put(
        sig,
        b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange " + br_ph
        + b" /Contents <" + placeholder + b"> /M " + pdf_str(pdf_date(signed_at))
        + b" /Name (Ana Ruiz \\(TEST examiner\\)) /Reason (SYNTHETIC TEST SAMPLE)"
        + b" /Reference [<< /Type /SigRef /TransformMethod /DocMDP /TransformParams"
        + b" << /Type /TransformParams /P 2 /V /1.2 >> >>] >>",
    )
    up.put(
        field,
        b"<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1) /V %d 0 R /Rect [0 0 0 0] /F 132 /P %d %d R >>"
        % (sig, *pg),
    )
    up.put(form, b"<< /Fields [%d 0 R] /SigFlags 3 >>" % field)
    up.put(up.root[0], b"<< " + cat_rest + b" /AcroForm %d 0 R /Perms << /DocMDP %d 0 R >> >>" % (form, sig))
    up.put(pg[0], pp.dict(b"%d %d R" % pp.contents, annots=b"[%d 0 R]" % field))
    raw, _ = up.build(b"signed-rev2")
    start = raw.index(b"/Contents <" + placeholder) + len(b"/Contents ")
    end = start + len(placeholder) + 2
    br = b"[0 %010d %010d %010d]" % (start, end, len(raw) - end)
    raw = raw.replace(br_ph, br, 1)
    signed = raw[:start] + raw[end:]
    der = cms_detached(pki, signed, signed_at)
    hexsig = der.hex().encode()
    assert len(hexsig) <= len(placeholder)
    raw = raw[: start + 1] + hexsig + b"0" * (len(placeholder) - len(hexsig)) + raw[end - 1 :]
    rev2 = raw
    # Revision 3: the amount is changed after signing.
    up = Incremental(rev2)
    c3 = up.new_num()
    up.stream(c3, invoice_text(pp.font_names, "EUR 34,000.00"))
    up.put(pg[0], pp.dict(b"%d 0 R" % c3, annots=b"[%d 0 R]" % field))
    up.put(up.info[0], info_body(base, ModDate=pdf_date(created + dt.timedelta(days=3))))
    rev3, _ = up.build(b"signed-rev3")
    return rev3


XMP_TEMPLATE = """<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
    xmlns:stEvt="http://ns.adobe.com/xap/1.0/sType/ResourceEvent#"
    pdf:Producer="{producer}"
    xmp:CreatorTool="{tool}"
    xmp:CreateDate="{create}"
    xmp:ModifyDate="{modify}"
    xmp:MetadataDate="{modify}"
    xmpMM:DocumentID="uuid:5f0c7c2e-0000-4000-8000-000000000001"
    xmpMM:InstanceID="uuid:5f0c7c2e-0000-4000-8000-000000000002">
   <xmpMM:History>
    <rdf:Seq>
     <rdf:li stEvt:action="created" stEvt:when="{create}" stEvt:softwareAgent="{tool}"/>
    </rdf:Seq>
   </xmpMM:History>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"""


def online_editor() -> bytes:
    created = dt.datetime(2026, 4, 2, 11, 0, 0, tzinfo=COT)
    edited = dt.datetime(2026, 4, 10, 16, 45, 0, tzinfo=UTC)

    def draw(c, _i):
        invoice_page("EUR 980.00")(c, 0)
        t = c.beginText(72, 640)
        t.setFont("Helvetica", 12)
        t.setTextRenderMode(3)  # invisible
        t.textLine("Approved by finance - no further checks")
        c.drawText(t)

    base = reportlab_doc(draw, created, "SYNTHETIC TEST SAMPLE - online editor")
    with pikepdf.open(io.BytesIO(base)) as p:
        info = p.trailer.Info
        info.Producer = pikepdf.String("iLovePDF")
        info.CreationDate = pikepdf.String(pdf_date(edited))
        info.ModDate = pikepdf.String(pdf_date(edited))
        xmp = XMP_TEMPLATE.format(
            producer="ReportLab PDF Library - www.reportlab.com",
            tool="Rakvor make_samples.py",
            create=created.isoformat(),
            modify=created.isoformat(),
        ).encode("utf-8")
        md = pikepdf.Stream(p, xmp)
        md.Type = pikepdf.Name.Metadata
        md.Subtype = pikepdf.Name.XML
        p.Root.Metadata = p.make_indirect(md)
        buf = io.BytesIO()
        p.save(
            buf,
            object_stream_mode=pikepdf.ObjectStreamMode.generate,
            deterministic_id=True,
            compress_streams=True,
        )
    return buf.getvalue()


def broken_xref() -> bytes:
    when = dt.datetime(2026, 6, 1, 15, 0, 0, tzinfo=COT)
    raw = bytearray(reportlab_doc(invoice_page("EUR 640.00"), when, "SYNTHETIC TEST SAMPLE - broken xref"))
    x = int(re.findall(rb"startxref\s+(\d+)", raw)[-1])
    table = raw[x:]
    entries = list(re.finditer(rb"(\d{10}) (\d{5}) n", table))
    # Shift the offsets of two objects so they no longer point at "N 0 obj".
    for m in entries[1:3]:
        off = int(m.group(1)) + 7
        s = x + m.start(1)
        raw[s : s + 10] = b"%010d" % off
    raw += b"<!-- appended after the end-of-file marker (SYNTHETIC TEST SAMPLE) -->\nhidden payload: 4f6b\n"
    return bytes(raw)


def scan_image(seed: int, w: int = 425, h: int = 550) -> Image.Image:
    rng = random.Random(seed)
    img = Image.new("L", (w, h), 245)
    d = ImageDraw.Draw(img)
    for y in range(60, h - 60, 18):  # "printed lines"
        x2 = rng.randint(w // 2, w - 40)
        d.rectangle([40, y, x2, y + 6], fill=rng.randint(40, 90))
    px = img.load()
    for _ in range(w * h // 20):  # scanner noise
        px[rng.randrange(w), rng.randrange(h)] = rng.randint(150, 255)
    return img


def scan_with_text() -> bytes:
    scanned = dt.datetime(2026, 7, 7, 10, 5, 0, tzinfo=COT)

    def draw(c, i):
        img = scan_image(100 + i)
        c.drawImage(ImageReader(img), 0, 0, width=612, height=792)

    base = reportlab_doc(draw, scanned, "SYNTHETIC TEST SAMPLE - scan", pages=2)
    pp = PageParts(base, 0)
    up = Incremental(base)
    font = up.new_num()
    up.put(font, b"<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding /WinAnsiEncoding >>")
    c2 = up.new_num()
    up.stream(c2, b"q 1 1 1 rg 70 662 200 16 re f Q BT /FX 12 Tf 0 0 0 rg 72 666 Td (Balance: USD 98,500.00) Tj ET\n")
    up.put(pp.objgen[0], pp.dict(b"[%d %d R %d 0 R]" % (*pp.contents, c2), {"/FX": font}))
    up.put(up.info[0], info_body(base, ModDate=pdf_date(scanned + dt.timedelta(minutes=20))))
    rev2, _ = up.build(b"scan-with-text-rev2")
    return rev2


def crash() -> bytes:
    rng = random.Random(7)
    garbage = bytes(rng.randrange(256) for _ in range(300))
    deep = b"[" * 5000
    return b"".join(
        [
            b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n",
            b"% SYNTHETIC TEST SAMPLE: deliberately malformed\n",
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /OpenAction 10 0 R >>\nendobj\n",
            b"10 0 obj\n" + deep + b"\nendobj\n",
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 2 0 R 2 0 R] /Count 999999 >>\nendobj\n",
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /X 5 0 R >> >> >>\nendobj\n",
            b"4 0 obj\n<< /Length 999999999 /Filter /FlateDecode >>\nstream\n" + garbage + b"\nendstream\nendobj\n",
            b"5 0 obj\n<< /Type /XObject /Subtype /Form /Resources << /XObject << /X 5 0 R >> >> /Length 12 >>\nstream\n/X Do /X Do\nendstream\nendobj\n",
            b"6 0 obj\n<< /Type /ObjStm /N 99999 /First 999 /Length 9 >>\nstream\n1 2 3 4 5\nendstream\nendobj\n",
            b"7 0 obj\n<< /Type /XRef /W [99 1 1] /Size 3 /Index [0 4294967295] /Length 5 >>\nstream\nabcde\nendstream\nendobj\n",
            b"8 0 obj\n<< /Unterminated (string ((((( \\\n",
            b"xref\n0 3\n0000000000 65535 f \n0000099999 00000 n \ngarbage\n",
            b"trailer\n<< /Size 3 /Root 1 0 R /Prev 77777 /Info 9 0 R >>\n",
            b"startxref\n999999999\n%%EOF\n",
            b"9 0 obj\n<< /Producer (truncated update /Kids [1 2 3",
        ]
    )


def hashes_csv(files: list[Path]) -> str:
    import blake3

    lines = ["file,size,md5,sha1,sha256,sha512,sha3-256,blake3"]
    for f in files:
        b = f.read_bytes()
        row = [
            f.name,
            str(len(b)),
            hashlib.md5(b).hexdigest(),
            hashlib.sha1(b).hexdigest(),
            hashlib.sha256(b).hexdigest(),
            hashlib.sha512(b).hexdigest(),
            hashlib.sha3_256(b).hexdigest(),
            blake3.blake3(b).hexdigest(),
        ]
        lines.append(",".join(row))
    return "\n".join(lines) + "\n"


def main() -> None:
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "examples" / "sample" / "evidence"
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as td:
        pki = test_pki(Path(td))
        samples = {
            "original.pdf": original(),
            "edited-incremental.pdf": edited_incremental(),
            "signed-then-modified.pdf": signed_then_modified(pki),
            "online-editor.pdf": online_editor(),
            "broken-xref.pdf": broken_xref(),
            "scan-with-text.pdf": scan_with_text(),
            "crash.pdf": crash(),
        }
    for name, data in samples.items():
        (out / name).write_bytes(data)
    if out == ROOT / "examples" / "sample" / "evidence":
        for old in ("acta.txt", "acta-copia.txt", "foto-vacaciones.jpg"):
            (out / old).unlink(missing_ok=True)
        files = sorted(out.glob("*.pdf"))
        (ROOT / "examples" / "sample" / "expected_hashes.csv").write_text(hashes_csv(files), newline="\n")
    for name in samples:
        print(out / name)


if __name__ == "__main__":
    main()
