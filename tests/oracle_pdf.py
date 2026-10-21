#!/usr/bin/env python3
"""Cross-checks the PDF layer against independent test oracles (never shipped):

  * pikepdf / libqpdf: page count of every revision prefix, trailer /ID, whether qpdf had to repair
    the file, and the merged xref of the final revision (`qpdf --show-xref`, when the qpdf CLI exists);
  * pyHanko: number of revisions and the startxref of each one (from the /Prev chain), and for
    every signature the digest ("intact"), the signature maths ("valid"), ByteRange coverage and
    DocMDP compliance.

  python tests/oracle_pdf.py [file.pdf ...]       default: examples/sample/evidence/*.pdf

Our side comes from `cargo run -p rv-pdfauth-pdf --example dump -- --tree <files>`.
Exit code 0 when every comparison agrees, 1 otherwise. A file an oracle cannot open is reported
and only counts as a disagreement when our analysis found nothing wrong with its structure.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import warnings
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

import pikepdf  # noqa: E402

try:
    from pyhanko.pdf_utils.reader import PdfFileReader
    from pyhanko.sign.validation import validate_pdf_signature
    from pyhanko_certvalidator import ValidationContext
except ImportError:  # pragma: no cover
    PdfFileReader = None


def ours(files: list[Path]) -> dict[str, dict]:
    env = dict(os.environ)
    cmd = ["cargo", "run", "-q", "-p", "rv-pdfauth-pdf", "--example", "dump", "--", "--tree", *map(str, files)]
    out = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)
    if out.returncode != 0:
        sys.exit(f"dump failed: {out.stderr[-2000:]}")
    res = {}
    for line in out.stdout.splitlines():
        d = json.loads(line)
        res[d["file"]] = d["report"]
    return res


def qpdf_xref(path: Path) -> dict[str, tuple[str, int]] | None:
    """Merged xref of the final revision as qpdf sees it: object -> ("file", offset) or ("objstm", stream)."""
    exe = shutil.which("qpdf")
    if exe is None:
        return None
    out = subprocess.run([exe, "--show-xref", str(path)], capture_output=True, text=True)
    if out.returncode not in (0, 3):  # 3 = success with warnings
        return None
    xref = {}
    for line in out.stdout.splitlines():
        m = re.match(r"(\d+)/(\d+): uncompressed; offset = (\d+)", line)
        if m:
            xref[f"{m[1]} {m[2]}"] = ("file", int(m[3]))
            continue
        m = re.match(r"(\d+)/(\d+): compressed; stream = (\d+)", line)
        if m:
            xref[f"{m[1]} {m[2]}"] = ("objstm", int(m[3]))
    return xref


def our_latest_offsets(rep: dict) -> dict[str, tuple[str, int]]:
    latest: dict[str, tuple[str, int]] = {}
    for rev in rep["object_tree"].get("revisions", []):
        for o in rev["objects"]:
            stm = o.get("in_object_stream")
            latest[o["id"]] = ("objstm", stm) if stm is not None else ("file", o["offset"])
    return latest


def damaged(rep: dict) -> bool:
    return any(not r["xref_consistent"] for r in rep["revisions"]) or not rep["revisions"]


class Check:
    def __init__(self, name: str):
        self.name = name
        self.lines: list[str] = []
        self.bad = 0

    def eq(self, what: str, mine, theirs) -> None:
        ok = mine == theirs
        self.bad += 0 if ok else 1
        self.lines.append(f"  {'ok ' if ok else 'BAD'} {what}: ours={mine!r} oracle={theirs!r}")

    def note(self, s: str) -> None:
        self.lines.append(f"  --  {s}")


def check_pikepdf(c: Check, path: Path, rep: dict) -> None:
    data = path.read_bytes()
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        try:
            with pikepdf.open(path) as p:
                c.eq("pikepdf page count (final revision)", rep["page_count"], len(p.pages))
                for _ in p.objects:  # qpdf parses lazily: resolve everything before reading its warnings
                    pass
                qpdf_warn = len(p.get_warnings())
        except Exception as e:  # noqa: BLE001
            c.note(f"pikepdf cannot open the file ({type(e).__name__}: {e})")
            c.eq("structure damaged (ours) vs pikepdf failure", damaged(rep), True)
            return
    c.eq("structure damaged (ours) vs qpdf repaired it", damaged(rep), qpdf_warn > 0)
    if damaged(rep):
        c.note(f"qpdf reported {qpdf_warn} warning(s) while repairing")
        return
    for r in rep["revisions"]:
        prefix = data[: r["end_offset"]]
        with pikepdf.open(__import__("io").BytesIO(prefix)) as p:
            c.eq(f"rev {r['index']} pages (pikepdf on revision prefix)", r["page_count"], len(p.pages))
            ids = [bytes(x).hex() for x in p.trailer.ID] if "/ID" in p.trailer else []
            c.eq(f"rev {r['index']} trailer /ID", r["id"], ids)
    xref = qpdf_xref(path)
    if xref is None:
        c.note("qpdf CLI not found or --show-xref failed: xref comparison skipped")
        return
    mine = our_latest_offsets(rep)
    diff = {k: (mine.get(k), v) for k, v in xref.items() if mine.get(k) != v}
    c.eq("qpdf --show-xref entries matching ours", len(xref) - len(diff), len(xref))
    for k, (a, b) in list(diff.items())[:5]:
        c.note(f"object {k}: ours {a}, qpdf {b}")


def check_pyhanko(c: Check, path: Path, rep: dict) -> None:
    if PdfFileReader is None:
        c.note("pyhanko not installed: skipped")
        return
    with open(path, "rb") as f:
        try:
            r = PdfFileReader(f, strict=False)
            total = r.xrefs.total_revisions
        except Exception as e:  # noqa: BLE001
            c.note(f"pyhanko cannot read the file ({type(e).__name__}: {e})")
            c.eq("structure damaged (ours) vs pyhanko failure", damaged(rep), True)
            return
        if damaged(rep):
            c.note(f"pyhanko read {total} revision(s) of a damaged file; ours: {len(rep['revisions'])}")
        else:
            c.eq("revision count", len(rep["revisions"]), total)
            for i in range(min(total, len(rep["revisions"]))):
                c.eq(f"rev {i + 1} startxref", rep["revisions"][i]["startxref"], r.xrefs.get_startxref_for_revision(i))
        sigs = list(r.embedded_signatures)
        c.eq("signature count", len(rep["signatures"]), len(sigs))
        vc = ValidationContext(trust_roots=[], allow_fetching=False)
        for mine, es in zip(rep["signatures"], sigs):
            st = validate_pdf_signature(es, vc)
            c.eq(f"{mine['field']} digest ok (pyhanko intact)", mine["digest_ok"], st.intact)
            c.eq(f"{mine['field']} signature ok (pyhanko valid)", mine["signature_ok"], st.valid)
            cov = getattr(st.coverage, "name", str(st.coverage))
            c.eq(f"{mine['field']} covers its revision", mine["covers_revision"], cov in ("ENTIRE_REVISION", "ENTIRE_FILE"))
            our_violation = any(f["code"] == "DOCMDP_VIOLATION" for f in rep["findings"])
            if mine["docmdp"] is not None and st.docmdp_ok is not None:
                c.eq(f"{mine['field']} DocMDP respected", not our_violation, bool(st.docmdp_ok))
            c.note(f"pyhanko: {st.summary()}; modification level {getattr(st.modification_level, 'name', None)}")


def main() -> int:
    files = [Path(a) for a in sys.argv[1:]] or sorted((ROOT / "examples" / "sample" / "evidence").glob("*.pdf"))
    files = [f.resolve() for f in files]
    reports = ours(files)
    total_bad = 0
    for f in files:
        rep = reports[str(f)]
        c = Check(f.name)
        check_pikepdf(c, f, rep)
        check_pyhanko(c, f, rep)
        total_bad += c.bad
        print(f"{'AGREE   ' if c.bad == 0 else 'DISAGREE'} {f.name}")
        print("\n".join(c.lines))
    print(f"\n{len(files)} file(s), {total_bad} disagreement(s)")
    return 0 if total_bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
