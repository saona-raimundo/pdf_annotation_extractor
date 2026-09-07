#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["pikepdf>=8"]
# ///
"""Add the DOCUMENT FURNITURE nobody creates by hand, and verify it.

    pdflatex base.tex && uv run make.py
    uv run make.py --verify          # verify only, change nothing

A /Widget form field and a /Link belong to the FILE, not to a reader's
annotations. No viewer creates them with an annotation tool, so they
have to be in base.pdf before it is handed to an annotator -- and they
have to be there, because IN6 asserts that none of them becomes a
record.

Why not hyperref. hyperref would give us \\href and \\TextField, but it
would also scatter /Link annotations across every page and over the
section headings, which would make the counts in IN1 to IN5
uncontrolled. Three objects on one page can be counted.

/Link is the case the corpus had been quietly avoiding: every base.tex
here excludes hyperref precisely so that /Link does not appear, which
meant nothing tested that we filter it. A hyperref'd paper carries a
/Link for every citation, cross-reference, URL and contents entry --
hundreds per paper, all with no useful text.

Both links are added with real actions, because a tool that filters on
the presence of /A rather than on /Subtype would pass a bare /Link and
fail these:

    /URI    an external address
    /GoTo   an internal destination, the commonest kind in a paper

Idempotent: re-running finds the furniture already present.

Requires: pdftotext (poppler-utils), pikepdf >= 8
"""

import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

import pikepdf
from pikepdf import Array, Dictionary, Name, String

_HERE = Path(__file__).resolve().parent
BASE = _HERE / "base.pdf"

WIDGET_ANCHOR = "IN6A"                    # field goes in this line's margin
URI_PHRASE = ["see", "the", "reference"]   # /Link with a /URI action
GOTO_PHRASE = ["go", "back", "to", "page", "one"]   # /Link with /GoTo

EXPECTED = {"/Widget": 1, "/Link": 2}


def words():
    """Every word with its box in user space, page by page."""
    xml = subprocess.run(["pdftotext", "-bbox", str(BASE), "-"],
                         capture_output=True, check=True, text=True).stdout
    h = "{http://www.w3.org/1999/xhtml}"
    out = []
    for pno, page in enumerate(ET.fromstring(xml).iter(f"{h}page"), 1):
        height = float(page.get("height"))
        for w in page.iter(f"{h}word"):
            out.append((pno, (w.text or ""),
                        float(w.get("xMin")), height - float(w.get("yMax")),
                        float(w.get("xMax")), height - float(w.get("yMin"))))
    return out


def phrase_box(ws, phrase):
    """Box around a run of consecutive words, matched on the same line."""
    stripped = [(p, t.strip(".,"), a, b, c, d) for p, t, a, b, c, d in ws]
    for i in range(len(stripped) - len(phrase) + 1):
        run = stripped[i:i + len(phrase)]
        if [r[1] for r in run] != phrase:
            continue
        if len({r[0] for r in run}) != 1 or max(abs(r[3] - run[0][3]) for r in run) > 1:
            continue          # split across a line break; try the next match
        return (run[0][0], min(r[2] for r in run), min(r[3] for r in run),
                max(r[4] for r in run), max(r[5] for r in run))
    sys.exit(f"phrase {' '.join(phrase)!r} not found on a single line")


def sentinel_row(ws, sentinel):
    for pno, t, x0, y0, x1, y1 in ws:
        if t == f"[[{sentinel}]]":
            return pno, y0, y1
    sys.exit(f"no line carries [[{sentinel}]]")


def tally(pdf):
    counts = {}
    for page in pdf.pages:
        for a in page.get("/Annots", []):
            k = str(a.get("/Subtype"))
            counts[k] = counts.get(k, 0) + 1
    return counts


def verify(pdf):
    counts = tally(pdf)
    problems = [f"{k}: found {counts.get(k, 0)}, expected {n}"
                for k, n in EXPECTED.items() if counts.get(k, 0) != n]
    if "/AcroForm" not in pdf.Root:
        problems.append("/AcroForm missing: the /Widget is unreachable and a "
                        "viewer may strip it on save")
    return problems, counts


def build():
    pdf = pikepdf.open(BASE, allow_overwriting_input=True)
    if not verify(pdf)[0]:
        print("furniture already present; nothing to do")
        return pdf

    ws = words()
    pno, y0, y1 = sentinel_row(ws, WIDGET_ANCHOR)
    page = pdf.pages[pno - 1]

    field = pdf.make_indirect(Dictionary(
        Type=Name.Annot, Subtype=Name.Widget, FT=Name.Tx,
        T=String("fixture-field"),
        V=String("a form field value, not a comment"),
        # Right margin, level with the IN6A line: clear of every target.
        Rect=Array([520, y0 - 2, 585, y1 + 2]),
        F=4, DA=String("/Helv 9 Tf 0 g"),
    ))

    up, ux0, uy0, ux1, uy1 = phrase_box(ws, URI_PHRASE)
    uri = pdf.make_indirect(Dictionary(
        Type=Name.Annot, Subtype=Name.Link,
        Rect=Array([ux0, uy0, ux1, uy1]),
        # /QuadPoints is LEGAL on a link (ISO 32000-1 Table 173) and
        # /Contents is in the common annotation dictionary, legal on
        # every subtype and used for alt text. Both are here on purpose:
        # a filter keyed on "has quads" or "has contents" rather than on
        # /Subtype leaks this annotation, and a link with neither would
        # be trivially excludable and would test nothing.
        QuadPoints=Array([ux0, uy1, ux1, uy1, ux0, uy0, ux1, uy0]),
        Contents=String("LINK ALT TEXT MUST NOT APPEAR"),
        Border=Array([0, 0, 0]),
        A=Dictionary(S=Name.URI, URI=String("https://example.org/reference")),
    ))

    gp, gx0, gy0, gx1, gy1 = phrase_box(ws, GOTO_PHRASE)
    goto = pdf.make_indirect(Dictionary(
        Type=Name.Annot, Subtype=Name.Link,
        Rect=Array([gx0, gy0, gx1, gy1]),
        # No quads and no contents on this one: the pair brackets the
        # two filter styles, so a tool that leaks one and not the other
        # says which key its filter is on.
        Border=Array([0, 0, 0]),
        Dest=Array([pdf.pages[0].obj, Name.XYZ, 0, 800, 0]),
    ))

    for target_page, objs in ((pno, [field]), (up, [uri]), (gp, [goto])):
        p = pdf.pages[target_page - 1]
        p["/Annots"] = pdf.make_indirect(
            Array(list(p.get("/Annots", [])) + objs))

    # /TU is the tooltip, which is what a real form carries; /Contents is
    # added too so that the widget also defeats a contents-keyed filter.
    field["/TU"] = String("WIDGET TOOLTIP MUST NOT APPEAR")
    field["/Contents"] = String("WIDGET CONTENTS MUST NOT APPEAR")

    # A /Widget not reachable from /AcroForm is non-conformant, will not
    # display in Acrobat, and is liable to be stripped by a viewer on
    # save -- which would quietly remove the thing IN6 is counting.
    pdf.Root["/AcroForm"] = pdf.make_indirect(Dictionary(
        Fields=Array([field]), DA=String("/Helv 9 Tf 0 g"),
        NeedAppearances=True,
    ))

    pdf.save()
    print(f"page {pno}: /Widget added beside [[{WIDGET_ANCHOR}]]")
    print(f"page {up}: /Link with a /URI action over {' '.join(URI_PHRASE)!r}")
    print(f"page {gp}: /Link with a /GoTo destination over {' '.join(GOTO_PHRASE)!r}")
    return pikepdf.open(BASE)


if __name__ == "__main__":
    if not BASE.exists():
        sys.exit(f"{BASE} not found; run: pdflatex base.tex")
    if not shutil.which("pdftotext"):
        sys.exit("pdftotext not found; install poppler-utils")
    pdf = pikepdf.open(BASE) if "--verify" in sys.argv else build()
    problems, counts = verify(pdf)
    if problems:
        print("FURNITURE MISMATCH:")
        for p in problems:
            print("   ", p)
        sys.exit(1)
    print("furniture ok: " + ", ".join(f"{k} x{v}" for k, v in sorted(counts.items())))
    print("next: annotate base.pdf by hand, following ANNOTATIONS.txt")
