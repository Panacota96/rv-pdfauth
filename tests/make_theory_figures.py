#!/usr/bin/env python3
"""Regenerates the figures of docs/theory.md from the synthetic samples in examples/sample/evidence.

  python tests/make_theory_figures.py                       # the two SVG byte maps (standard library only)
  python tests/make_theory_figures.py --bin <rv-pdfauth>    # + the visual-diff PNG (needs PDFium and Pillow)

Outputs (docs/images/theory/):
  revisions-edited-incremental.svg   where each revision of edited-incremental.pdf starts and ends, and the
                                     byte-exact prefixes the tool extracts (with their real SHA-256)
  byterange-signed-then-modified.svg the /ByteRange of the signature in signed-then-modified.pdf
  visual-diff-rev1-vs-rev2.png       top of the comparison image the tool writes for edited-incremental.pdf,
                                     revision 1 vs 2 (cropped and scaled down to keep the repository small)

The SVGs are drawn from a simple keyword scan of the sample files. It is an illustration for readers, not the
tool's parser; for these two simple samples its offsets equal the revision table of the technical report
(start/end offsets) and the SHA-256 values equal those of revisions/rev-NN.pdf. The PNG is taken from a real
`rv-pdfauth analyze` run (RV_NO_TIMESTAMP=1, so nothing is sent anywhere).
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE = ROOT / "examples" / "sample" / "evidence"
OUT = ROOT / "docs" / "images" / "theory"

INK = "#0D1117"
VIOLET = "#6B00FF"
TEAL = "#007C91"
GREY = "#8A9099"
LIGHT = ["#EDE4FF", "#DDF3F7", "#FFF1D6", "#E8F5E1"]
FONT = "font-family='Segoe UI, Helvetica, Arial, sans-serif'"


def esc(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def eof_ends(data: bytes) -> list[tuple[int, int]]:
    """(offset of %%EOF, end of its line) for every marker, like the tool's linear scan for this simple file."""
    out = []
    for m in re.finditer(rb"%%EOF", data):
        end = m.end()
        if data[end:end + 2] == b"\r\n":
            end += 2
        elif data[end:end + 1] in (b"\r", b"\n"):
            end += 1
        out.append((m.start(), end))
    return out


def text(x: float, y: float, s: str, size: int = 13, color: str = INK, anchor: str = "start", weight: str = "normal") -> str:
    return (f"<text x='{x:.1f}' y='{y:.1f}' {FONT} font-size='{size}' fill='{color}' text-anchor='{anchor}' "
            f"font-weight='{weight}'>{esc(s)}</text>")


def svg(width: int, height: int, body: list[str], title: str) -> str:
    return "\n".join([
        f"<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{height}' viewBox='0 0 {width} {height}' "
        f"role='img' aria-label='{esc(title)}'>",
        f"<title>{esc(title)}</title>",
        f"<rect width='{width}' height='{height}' fill='#FFFFFF'/>",
        *body,
        "</svg>",
        "",
    ])


def revisions_figure() -> str:
    data = (EVIDENCE / "edited-incremental.pdf").read_bytes()
    ends = eof_ends(data)
    size = len(data)
    x0, w = 40, 880
    sx = lambda off: x0 + w * off / size  # noqa: E731
    body = [text(x0, 28, "edited-incremental.pdf: one file, three saved revisions (byte offsets)", 16, weight="bold")]
    starts = [0] + [e for _, e in ends[:-1]]
    y = 50
    for i, ((eof, end), start) in enumerate(zip(ends, starts), 1):
        body.append(f"<rect x='{sx(start):.1f}' y='{y}' width='{sx(end) - sx(start):.1f}' height='54' "
                    f"fill='{LIGHT[(i - 1) % 4]}' stroke='{INK}' stroke-width='1'/>")
        body.append(text((sx(start) + sx(end)) / 2, y + 22, f"revision {i}", 14, anchor="middle", weight="bold"))
        body.append(text((sx(start) + sx(end)) / 2, y + 42, f"bytes {start}–{end - 1}", 12, anchor="middle"))
        seg = data[start:end]
        for kw, color in ((rb"\bxref\b", TEAL), (rb"startxref", VIOLET)):
            m = re.search(kw, seg)
            if m:
                xx = sx(start + m.start())
                body.append(f"<line x1='{xx:.1f}' y1='{y}' x2='{xx:.1f}' y2='{y + 54}' stroke='{color}' stroke-width='2'/>")
        body.append(text(sx(end) - 2, y + 70, f"%%EOF @ {eof}", 11, VIOLET, anchor="end"))
    body.append(text(x0, y + 92, "Teal line: xref table · violet line: startxref · each revision ends after its %%EOF line.",
                     12, GREY))
    y = 168
    body.append(text(x0, y, "What the tool extracts: revisions/rev-NN.pdf = the first N bytes of the evidence", 15, weight="bold"))
    for i, (_, end) in enumerate(ends, 1):
        yy = y + 16 + (i - 1) * 40
        digest = hashlib.sha256(data[:end]).hexdigest()
        body.append(f"<rect x='{x0}' y='{yy}' width='{sx(end) - x0:.1f}' height='26' fill='{LIGHT[(i - 1) % 4]}' "
                    f"stroke='{INK}' stroke-width='1'/>")
        body.append(text(x0 + 8, yy + 18, f"rev-{i:02d}.pdf = bytes 0–{end - 1}", 12, weight="bold"))
        body.append(text(sx(end) + 8 if i < len(ends) else x0 + w - 4, yy + 18, f"SHA-256 {digest[:16]}…", 12, TEAL,
                         anchor="start" if i < len(ends) else "end"))
    return svg(960, y + 16 + len(ends) * 40 + 10, body,
               "Byte map of the three revisions of edited-incremental.pdf and the prefixes extracted by rv-pdfauth")


def byterange_figure() -> str:
    data = (EVIDENCE / "signed-then-modified.pdf").read_bytes()
    m = re.search(rb"/ByteRange\s*\[\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s*\]", data)
    if not m:
        sys.exit("no /ByteRange in signed-then-modified.pdf")
    a, b, c, d = (int(v) for v in m.groups())
    ends = [e for _, e in eof_ends(data)]
    size = len(data)
    x0, w = 40, 880
    sx = lambda off: x0 + w * off / size  # noqa: E731
    y = 78
    parts = [
        (a, a + b, LIGHT[0], "signed bytes 1", f"{a}–{a + b - 1}"),
        (a + b, c, "#F2F2F2", "/Contents <…> (the CMS signature itself)", f"{a + b}–{c - 1}"),
        (c, c + d, LIGHT[0], "signed bytes 2", f"{c}–{c + d - 1}"),
        (c + d, size, LIGHT[2], "revision 3: NOT covered", f"{c + d}–{size - 1}"),
    ]
    body = [text(x0, 28, f"signed-then-modified.pdf: /ByteRange [{a} {b} {c} {d}]", 16, weight="bold")]
    for i, (s, e, fill, label, rng) in enumerate(parts):
        body.append(f"<rect x='{sx(s):.1f}' y='{y}' width='{max(sx(e) - sx(s), 1):.1f}' height='50' fill='{fill}' "
                    f"stroke='{INK}' stroke-width='1'/>")
        ly = y + 72 + (i % 2) * 34
        cx = (sx(s) + sx(e)) / 2
        anchor = "start" if cx < 200 else ("end" if cx > 800 else "middle")
        body.append(f"<line x1='{cx:.1f}' y1='{y + 50}' x2='{cx:.1f}' y2='{ly - 12}' stroke='{GREY}' stroke-width='1'/>")
        body.append(text(cx, ly, label, 12, weight="bold", anchor=anchor))
        body.append(text(cx, ly + 15, f"bytes {rng}", 11, GREY, anchor=anchor))
    for i, e in enumerate(ends, 1):
        xx = sx(e)
        top = y - 10 - (i % 2 == 0) * 16
        body.append(f"<line x1='{xx:.1f}' y1='{top}' x2='{xx:.1f}' y2='{y + 50}' stroke='{VIOLET}' stroke-width='2' "
                    f"stroke-dasharray='4 3'/>")
        body.append(text(xx - 4, top + 2, f"end of rev {i} ({e})", 11, VIOLET, anchor="end"))
    body.append(text(x0, y + 150, f"a + b = {a + b} = start of /Contents · c + d = {c + d} = end of revision 2: "
                     "the signature covers revision 2 exactly.", 12))
    body.append(text(x0, y + 168, "Everything appended later (revision 3) is outside the signed bytes and is "
                     "classified by the tool, not by the signature.", 12))
    return svg(960, y + 186, body, "ByteRange of the signature in signed-then-modified.pdf")


def visual_figure(binary: str) -> Path | None:
    try:
        from PIL import Image  # type: ignore
    except ImportError:
        print("Pillow not installed: visual-diff figure skipped", file=sys.stderr)
        return None
    with tempfile.TemporaryDirectory(prefix="rv-theory-") as tmp:
        env = {**os.environ, "RV_NO_TIMESTAMP": "1"}
        res = subprocess.run([binary, "analyze", "-i", str(EVIDENCE / "edited-incremental.pdf"), "--progress", "none",
                              "--out", tmp, "--case", "THEORY-FIGURES", "--actor", "make_theory_figures"],
                             env=env, capture_output=True, text=True)
        if res.returncode not in (0, 3):
            sys.exit(f"rv-pdfauth failed ({res.returncode}): {res.stderr[-400:]}")
        pngs = sorted(Path(tmp).glob("reports/*/items/*/visual/rev-01-vs-02-page-001.png"))
        if not pngs:
            print("no visual-diff image (PDFium not found? set RV_PDFIUM_PATH): figure skipped", file=sys.stderr)
            return None
        img = Image.open(pngs[0]).convert("RGB")
        top = img.crop((0, 0, img.width, min(img.height, 300)))
        scale = 960 / top.width
        top = top.resize((960, round(top.height * scale)), Image.LANCZOS)
        out = OUT / "visual-diff-rev1-vs-rev2.png"
        top.save(out, optimize=True)
        return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", help="rv-pdfauth executable (with PDFium reachable) to regenerate the visual-diff PNG")
    a = ap.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "revisions-edited-incremental.svg").write_text(revisions_figure(), encoding="utf-8", newline="\n")
    (OUT / "byterange-signed-then-modified.svg").write_text(byterange_figure(), encoding="utf-8", newline="\n")
    written = ["revisions-edited-incremental.svg", "byterange-signed-then-modified.svg"]
    if a.bin:
        p = visual_figure(a.bin)
        if p:
            written.append(p.name)
    for name in written:
        print(OUT / name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
