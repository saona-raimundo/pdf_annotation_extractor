# Producer behaviour

Observed behaviour of the programs used to annotate `base.pdf`. This file is
**descriptive**. Nothing here relaxes `expected.json`, and nothing here is
consumed by the test harness. Its purpose is to explain *why* an item fails
and to give an escalation a place to start.

A failing item is a defect in our extractor, a defect in the producer, or an
ambiguity in the PDF specification. Deciding which is the point of this file.

---

## Papers (GNOME Document Viewer) 50.2 — Bluefin

File: `producers/base-papers-50.2.pdf`. All 20 required items present.

### Conformance

| Property | Observed |
|---|---|
| `/QuadPoints` | Absolute, default user space. Conformant. |
| `/Rect` | Present and non-degenerate. |
| `/AP` appearance streams | **Absent on every annotation.** |
| `/Contents` encoding | UTF-16BE with BOM. The one empty comment (E1B) written with no BOM. |
| `/T` author | System user name, overriding the requested value — except E5B, where a per-annotation author was honoured. |
| `/Popup` | One per markup annotation. |
| `/NM`, `/RC`, `/IRT` | None written. |
| Save method | Incremental update; the file accumulated ~25 `%%EOF` markers over repeated saves. |
| PDF version | 1.5 |

Absent `/AP` is worth noting for extractors that read appearance streams
rather than quads. It is permitted — ISO 32000-1 makes `/AP` optional for
markup annotations — but it is a divergence from what most commercial tools
write, so a fixture from Papers alone will not catch `/AP`-related bugs.

### Quads per annotation

```
E1A 1   E1B 1   E1C 1   E2A 5   E2B 4
E3A 2   E3B 1   E3C 3   E4A 1   E4B 1
E5A 2   E5B 1   E5C 0   E6A 1   E6B 1
E6C 1   E7A 2   E7B 2   E8A 37  E9A 2
```

E2A's five quads for a five-line selection is the expected shape. E5C has
none, correctly, being a sticky note.

### E8A — 37 quads on the rotated page

On the `/Rotate 90` page Papers emitted **37 quads for a 34-character
selection**, roughly one per glyph, where every other item on an unrotated
page got one quad per line.

This is the one genuine inconsistency found so far, and it is compound:

1. **Our extractor.** Per-glyph quads are legitimate input. `text_under_quads`
   joins each band with a space, so 37 single-glyph bands would render as
   `a h i g h l i g h t`. Bands sharing a baseline should merge with no
   separator unless there is a real horizontal gap. **This is our bug and it
   is fixable regardless of rotation.**2. **Rotation.** Independently, our extractor warns and does not handle
   `/Rotate 90` at all. Also our bug.
3. **Papers.** Whether per-glyph quads are a Papers defect is unclear. They
   are spec-conformant, merely unusual. Needs a second producer on the same
   page to tell whether rotation causes it or Papers does it generally.

pdfannots 0.5 also recovers nothing here (`WARNING: Missing text for Highlight
annotation at page #3`), so the case is unhandled by both implementations.

**Not escalated yet** — decompose first. Annotating page 3 with a second tool
answers whether the 37 quads are inherent to rotated selections.

### Minor observations, not defects

- pdfannots rejects Papers' CMYK `/C [0 0 1 0]` with `WARNING: Invalid color`,
  then extracts the annotation correctly. A small bug in pdfannots, not here.
- pdfannots renders a `/Text` annotation (E5C) with an empty quote block
  before the comment, implying covered text where there is none.

---

## Escalation log

The single record of what is unfinished. The test suite allowlists nothing:
every item in `expected.json` is checked on every run, so while any entry here
is open, `cargo test` is red. That is the intended state — a red suite with an
explained entry below, rather than a green suite hiding a suppression.

| Date | Target | Issue | Status |
|---|---|---|---|
| — | our crate | Bands sharing a baseline joined with a space, corrupting per-glyph quad input (E8A) | open |
| — | our crate | `/Rotate` pages unsupported; warns and skips (E8A) | open |
| — | pdf_oxide | `geometry::Rect` documents `y` as the top-left corner; `extractors/text.rs` assigns the baseline to both `bbox.y` and `origin_y` | not filed |
| — | pdf_oxide | `/Contents` decoded with `from_utf8_lossy`, mangling the UTF-16BE strings that ISO 32000-1 §7.9.2.2 permits | not filed |
| — | Papers | Per-glyph `/QuadPoints` on rotated pages | needs a second producer first |