#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pypdf>=5"]
# ///
"""BUILD base.pdf and verify its 17-page attribute grid.

    pdflatex base.tex && uv run make.py
    uv run make.py --verify-grid     # verify only, change nothing

This family has TWO scripts and they do different jobs:

    make.py    acts on base.pdf, the fixture. Moves /MediaBox off PG15
               and /Rotate off PG16, then verifies that all seventeen
               pages carry the attributes base.tex says they do. Run it
               once, after pdflatex.

    check.py   acts on producers/*.pdf, the annotated files. Verifies
               that each highlight's quads landed on the target box.
               Run it after every annotation session.

The flag here used to be --check, which made "check" mean two things in
one directory. It is --verify-grid now.

The script carries PEP 723 inline metadata, so `uv run` resolves pypdf
into a throwaway environment and nothing has to be installed. It works
from any directory -- paths are relative to this file, not the shell:

    uv run tests/page/make.py --verify-grid

Without uv:  pip install "pypdf>=5" && python3 make.py

pdftex writes /MediaBox into every page dictionary and writes /Rotate
wherever \\pdfpageattr asks for one. ISO 32000-1 Table 30 makes both
inheritable, so a page dictionary may legally omit them and take the
value from /Pages -- and a tool that reads the page dictionary only
loses the page with no error at all. Moving one key up for PG15 and
another for PG16 is the whole of the transformation; everything else in
this family comes straight out of pdflatex.

Why pypdf and not pikepdf: qpdf, which pikepdf wraps, calls
pushInheritedAttributesToPage() and writes the inheritable keys back
into every page dictionary on save. The edit succeeds in memory and is
silently undone by the write. pypdf leaves the page tree as given.

For the same reason, verify against the raw page dictionary rather than
through a wrapper that resolves inheritance -- reading page.get() after
a deletion still returns a value, and that is inheritance working, not
a failed edit. --verify-grid does it the right way.

Requires: pypdf >= 5
"""

import sys
from pathlib import Path

import pypdf
from pypdf.generic import NameObject, NumberObject

# Relative to THIS FILE, not the shell's working directory, so the
# script works from anywhere:  uv run tests/page/make.py
BASE = Path(__file__).resolve().parent / "base.pdf"

MEDIABOX_PAGE = 15   # 1-based, carries [[PG15]]
ROTATE_PAGE = 16     # 1-based, carries [[PG16]]
INHERITED_ROTATE = 90

# The grid from base.tex, as EFFECTIVE values after inheritance.
#   page: (rotate, crop_inset, mediabox_origin, extra)
GRID = {
    1:  (0,   None, 0,  None),      2:  (90,  None, 0,  None),
    3:  (180, None, 0,  None),      4:  (270, None, 0,  None),
    5:  (0,   20,   0,  None),      6:  (90,  20,   0,  None),
    7:  (180, 20,   0,  None),      8:  (270, 20,   0,  None),
    9:  (0,   None, 20, None),      10: (90,  None, 20, None),
    11: (180, None, 20, None),      12: (270, None, 20, None),
    13: (0,   20,   20, None),      14: (-90, None, 0,  None),
    15: (0,   None, 0,  "mediabox inherited"),
    16: (90,  None, 0,  "rotate inherited"),
    17: (0,   20,   0,  "trimbox present"),
}


def leaves(node):
    """Page dictionaries in document order, without going through .pages
    (whose wrapper resolves inherited attributes)."""
    if node.get("/Type") == "/Page":
        yield node
        return
    for kid in node["/Kids"]:
        yield from leaves(kid.get_object())


def raw(page, key):
    try:
        return page.raw_get(key)
    except KeyError:
        return None


def check(path=None):
    reader = pypdf.PdfReader(path or BASE)
    root = reader.trailer["/Root"]["/Pages"]
    pages = list(leaves(root))
    problems = []

    if len(pages) != len(GRID):
        return [f"expected {len(GRID)} pages, found {len(pages)}"]

    for n, page in enumerate(pages, 1):
        rot, crop, origin, extra = GRID[n]
        own_rot, own_mb = raw(page, "/Rotate"), raw(page, "/MediaBox")

        if n == ROTATE_PAGE:
            if own_rot is not None:
                problems.append(f"PG{n:02d}: /Rotate still on the page dictionary")
            if int(root.get("/Rotate", -1)) != INHERITED_ROTATE:
                problems.append(f"PG{n:02d}: /Pages carries no /Rotate {INHERITED_ROTATE}")
        elif own_rot is None or int(own_rot) != rot:
            problems.append(f"PG{n:02d}: /Rotate is {own_rot}, expected {rot}")

        if n == MEDIABOX_PAGE:
            if own_mb is not None:
                problems.append(f"PG{n:02d}: /MediaBox still on the page dictionary")
            if "/MediaBox" not in root:
                problems.append(f"PG{n:02d}: /Pages carries no /MediaBox")
        elif own_mb is None:
            problems.append(f"PG{n:02d}: /MediaBox missing")
        elif float(own_mb[0]) != origin:
            problems.append(f"PG{n:02d}: /MediaBox origin {float(own_mb[0])}, expected {origin}")

        cb = raw(page, "/CropBox")
        if crop is None and cb is not None:
            problems.append(f"PG{n:02d}: unexpected /CropBox {list(cb)}")
        if crop is not None:
            if cb is None:
                problems.append(f"PG{n:02d}: /CropBox missing")
            elif abs(float(cb[0]) - (origin + crop)) > 0.01:
                problems.append(f"PG{n:02d}: /CropBox starts at {float(cb[0])}, "
                                f"expected {origin + crop}")

        has_trim = raw(page, "/TrimBox") is not None
        if has_trim != (extra == "trimbox present"):
            problems.append(f"PG{n:02d}: /TrimBox {'present' if has_trim else 'absent'}, "
                            "which is not what the grid says")

    labels = reader.trailer["/Root"].get("/PageLabels")
    if labels is None:
        problems.append("catalog carries no /PageLabels")
    else:
        nums = [int(x) for x in labels["/Nums"][::2]]
        if nums != [1, 4, 10, 14]:
            problems.append(f"/PageLabels ranges start at {nums}, expected [1, 4, 10, 14]")
    return problems


def build():
    writer = pypdf.PdfWriter(clone_from=BASE)
    root = writer._root_object["/Pages"]
    pages = list(leaves(root))
    if len(pages) != len(GRID):
        sys.exit(f"expected {len(GRID)} pages, found {len(pages)}")

    mb_page = pages[MEDIABOX_PAGE - 1]
    own = raw(mb_page, "/MediaBox")
    if own is not None:
        if "/MediaBox" not in root:
            root[NameObject("/MediaBox")] = own
        elif [float(v) for v in root["/MediaBox"]] != [float(v) for v in own]:
            sys.exit(f"PG{MEDIABOX_PAGE}: /MediaBox differs from the inheritable one; "
                     "deleting it would move the page")
        del mb_page[NameObject("/MediaBox")]
        print(f"PG{MEDIABOX_PAGE}: /MediaBox moved to /Pages")

    rot_page = pages[ROTATE_PAGE - 1]
    root[NameObject("/Rotate")] = NumberObject(INHERITED_ROTATE)
    if raw(rot_page, "/Rotate") is not None:
        del rot_page[NameObject("/Rotate")]
    print(f"PG{ROTATE_PAGE}: /Rotate {INHERITED_ROTATE} put on /Pages, absent from the page")

    # /Pages now carries /Rotate, so every OTHER page needs its own or it
    # would inherit 90 too. base.tex writes one on all sixteen; check.
    for n, page in enumerate(pages, 1):
        if n != ROTATE_PAGE and raw(page, "/Rotate") is None:
            sys.exit(f"PG{n:02d} has no /Rotate of its own and would inherit "
                     f"{INHERITED_ROTATE}. \\pdfpageattr is sticky -- give every "
                     "page its own, even an empty one.")

    with open(BASE, "wb") as f:
        writer.write(f)


if __name__ == "__main__":
    if not BASE.exists():
        sys.exit(f"{BASE} not found; run: pdflatex base.tex")
    if "--verify-grid" not in sys.argv:
        build()
    bad = check()
    if bad:
        print("GRID MISMATCH:")
        for b in bad:
            print("   ", b)
        sys.exit(1)
    print(f"grid ok: {len(GRID)} pages match base.tex")
    print("next: annotate base.pdf")
