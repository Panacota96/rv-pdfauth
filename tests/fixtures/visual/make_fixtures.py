"""Writes the small synthetic PDFs used by the render/visual-diff and worker tests (standard library only).

    python tests/fixtures/visual/make_fixtures.py

Output is deterministic (fixed dates and IDs), so re-running it must not change the committed files:
  one-revision.pdf    1 page, 1 revision
  two-revisions.pdf   revision 1 = one-revision.pdf byte for byte; revision 2 is an incremental update that
                      changes the total on page 1 (100.00 -> 900.00 EUR), adds page 2 and changes /Producer.
Every revision ends with "%%EOF\\n", so revision N = file[..end of its %%EOF line].
"""

from pathlib import Path

HERE = Path(__file__).resolve().parent
ID = "<5256464958545552455331303030303031><5256464958545552455331303030303031>"


def stream(content: bytes) -> bytes:
    return b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream"


def page_content(total: str, extra: bytes = b"") -> bytes:
    return (
        b"BT /F1 16 Tf 20 160 Td (INVOICE 2026-117) Tj 0 -40 Td (CLIENT: EXAMPLE SAS) Tj "
        b"0 -40 Td (TOTAL " + total.encode() + b" EUR) Tj ET" + extra
    )


def write_objects(out: bytearray, objects: dict[int, bytes]) -> dict[int, int]:
    offsets = {}
    for num, body in objects.items():
        offsets[num] = len(out)
        out += b"%d 0 obj\n" % num + body + b"\nendobj\n"
    return offsets


def xref(offsets: dict[int, int], with_free_head: bool) -> bytes:
    """Classic xref table with one subsection per run of consecutive object numbers."""
    entries = dict(offsets)
    lines = []
    nums = sorted(entries)
    if with_free_head:
        nums = [0] + nums
    i = 0
    while i < len(nums):
        j = i
        while j + 1 < len(nums) and nums[j + 1] == nums[j] + 1:
            j += 1
        lines.append(b"%d %d\n" % (nums[i], j - i + 1))
        for n in nums[i : j + 1]:
            if n == 0:
                lines.append(b"0000000000 65535 f \n")
            else:
                lines.append(b"%010d 00000 n \n" % entries[n])
        i = j + 1
    return b"xref\n" + b"".join(lines)


def revision_1() -> tuple[bytearray, int]:
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    objs = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        4: stream(page_content("100.00")),
        5: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        6: b"<< /Producer (RV fixture writer 1.0) /CreationDate (D:20260101120000Z) >>",
    }
    offsets = write_objects(out, objs)
    start = len(out)
    out += xref(offsets, True)
    out += b"trailer\n<< /Size 7 /Root 1 0 R /Info 6 0 R /ID [" + ID.encode() + b"] >>\n"
    out += b"startxref\n%d\n%%%%EOF\n" % start
    return out, start


def revision_2(base: bytearray, prev_xref: int) -> bytearray:
    out = bytearray(base)
    objs = {
        2: b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>",
        4: stream(page_content("900.00", b" 0.8 0.1 0.1 rg 200 20 80 30 re f")),
        6: b"<< /Producer (Other Editor 9.1) /CreationDate (D:20260101120000Z) /ModDate (D:20260315093000Z) >>",
        7: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 8 0 R >>",
        8: stream(b"BT /F1 16 Tf 20 160 Td (ANNEX ADDED LATER) Tj ET"),
    }
    offsets = write_objects(out, objs)
    start = len(out)
    out += xref(offsets, True)
    out += b"trailer\n<< /Size 9 /Root 1 0 R /Info 6 0 R /Prev %d /ID [" % prev_xref + ID.encode() + b"] >>\n"
    out += b"startxref\n%d\n%%%%EOF\n" % start
    return out


def main() -> None:
    one, xref1 = revision_1()
    two = revision_2(one, xref1)
    (HERE / "one-revision.pdf").write_bytes(bytes(one))
    (HERE / "two-revisions.pdf").write_bytes(bytes(two))
    print(f"one-revision.pdf {len(one)} bytes; two-revisions.pdf {len(two)} bytes (revision 1 ends at {len(one)})")


if __name__ == "__main__":
    main()
