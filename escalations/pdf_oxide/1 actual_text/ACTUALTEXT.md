# `/ActualText` is re-encoded through the font in character mode

**Crate:** `pdf_oxide` 0.3.77 (latest on crates.io, 2026-09-08)
**Affects:** `extract_chars`, and every API that reports per-glyph geometry
**Repro:** `actualtext-min.pdf`, 1055 bytes, beside this file
**Verified:** by reading `src/extractors/text.rs` and by running the 0.3.77
Python wheel, which shares the extractor

---

## 1. Summary

A marked-content sequence carrying `/ActualText` declares what the whole run
of glyphs inside it means (ISO 32000-1 §14.9.4). `extract_chars` reads the key
and honours the scope rules, but materialises the declared string by feeding
the *already decoded* Unicode back through the font as character codes. The
declared text is therefore re-encoded, re-decoded through the font's encoding,
and typeset with the replacement's own metrics rather than the span's.

`extract_text` and `extract_spans` get the text right. The defect is confined
to character mode, and the correct code is in the same function.

## 2. What already works

Worth stating, because the fix should not disturb it:

* Both string forms are read: hex `<FEFF…>` and literal `()`.
* The UTF-16BE byte order mark is stripped.
* Scope semantics are right: the replacement is emitted once per sequence,
  subsequent show operators inside it are suppressed, and the position still
  advances by the original text — so glyphs *after* the span land correctly.
* An empty declaration suppresses its glyph, which is what the key means.
* Span mode returns the declared string intact, in the right place in the
  reading order.

## 3. The defect

`src/extractors/text.rs`, in the `Tj` handler's `/ActualText` branch:

```rust
if self.extract_spans {
    // Span mode: push the pre-decoded Unicode straight into the buffer,
    // bypassing font character mapping.
    …
} else {
    // Character mode: show_text maps through font, but ActualText
    // is already decoded. Fall back to show_text for positioning.
    self.show_text(actual_text.as_bytes())?;
}
```

`actual_text` is a `String`. `as_bytes()` is its UTF-8 encoding, and
`show_text` interprets bytes as character codes in the current font. Three
symptoms follow from that one call, which is why this reads as three unrelated
bugs in a bug tracker:

1. **The text is re-encoded.** `™` is `E2 84 A2`, so three glyphs are looked
   up and three characters come back. What they are depends on the font's
   encoding, which is why the same defect produces different garbage in every
   document: under `WinAnsiEncoding` it is `â„¢`; in a Latin Modern subset
   whose `/ToUnicode` maps code 32 to U+2423 OPEN BOX, a declared *space*
   becomes a visible-blank symbol.

2. **The replacement is typeset with its own metrics.** It starts at the span's
   origin and advances by the widths of *its* glyphs, so it occupies a
   different width from the span it stands for. Seven characters over a narrow
   `§` overrun into the following word, and since the char list is sorted by
   position, the two interleave.

3. **A span crossing a line break is typeset entirely on the first line.**
   Only one show operator emits, so the whole declared string lands on the
   first line's baseline and nothing appears on the second.

## 4. Minimal example

One page, `Helvetica` with `/WinAnsiEncoding` — no embedded font and no
`/ToUnicode`, so nothing in the file can be blamed for the mojibake. Three
declarations, one per symptom. Content stream verbatim:

```
BT /F1 12 Tf 20 160 Td [(the )]TJ ET
/Span<</ActualText<FEFF2122>>>BDC                       % declares "™"
BT /F1 12 Tf 42.7 160 Td [(\(TM\))]TJ ET
EMC
BT /F1 12 Tf 20 130 Td [(the )]TJ ET
/Span<</ActualText<FEFF00530065006300740069006F006E>>>BDC   % declares "Section"
BT /F1 12 Tf 42.7 130 Td [(\247)]TJ ET                  % one glyph: §
EMC
BT /F1 12 Tf 49.4 130 Td [( on rounding)]TJ ET
/Span<</ActualText<FEFF00460069006700750072006500200033>>>BDC  % declares "Figure 3"
BT /F1 12 Tf 20 100 Td [(third figure)]TJ 0 -20 Td [(of the set)]TJ ET
EMC
BT /F1 12 Tf 84 80 Td [( is cited here)]TJ ET
```

### Expected

Each declared span reads as its declaration, once:

```
the ™
the Section on rounding
Figure 3 is cited here
```

### Observed

`extract_text` (span mode) is correct:

```
'the ™\n\nthe Section on rounding\n\nFigure 3\n is cited here'
```

`extract_chars` (character mode), concatenated in the order returned:

```
'the â„¢the S eocnt ioronundingFigure 3 is cited here'
```

Symptom by symptom, from the returned `TextChar`s:

**1. Re-encoding.** The declaration is one character; three come back, and
their code points are the UTF-8 bytes read through `WinAnsiEncoding`
(`0x84` → U+201E is why the middle one is a low-9 quotation mark):

| char | code point | x | advance |
|---|---|---|---|
| `â` | U+00E2 | 42.70 | 6.60 |
| `„` | U+201E | 49.30 | 6.60 |
| `¢` | U+00A2 | 55.90 | 6.60 |

**2. Overrun and interleaving.** The `§` is 6.7pt wide and sits at x=42.70;
`" on rounding"` follows at x=49.40. The seven declared characters are typeset
across 42.70–82.72, straight through it:

```
declared:  S 42.70   e 50.70   c 57.38   t 63.38   i 66.71   o 69.38   n 76.05
real:        ' ' 49.40   o 52.74   n 59.41   ' ' 66.08   r 69.42   o 73.41   u 80.08 …
sorted:    S ' ' e o c n t ' ' i o r o n u …   →   "S eocnt ioronunding"
```

**3. Line break.** All eight characters of `Figure 3` are at y=100. The span's
second-line glyphs (`of the set`, y=80) are suppressed, correctly, but nothing
replaces them; and the declared run ends at x=64.0 while the real ink it stands
for ends at x≈85, so a consumer intersecting a selection rectangle with the
glyphs recovers a truncated prefix.

### Commands

```console
$ python -c "import pdf_oxide as po; d=po.PdfDocument('actualtext-min.pdf'); \
             print(repr(d.extract_text(0)))"
$ python -c "import pdf_oxide as po; d=po.PdfDocument('actualtext-min.pdf'); \
             print(repr(''.join(c.char for c in d.extract_chars(0))))"
```

## 5. The existing APIs, and why none of them is a way round it

We need two things together: the declared string, and where on the page the
glyphs it replaces are. Every entry point gives one or the other. Measured on
the minimal file:

| API | declared text | geometry of the declared run |
|---|---|---|
| `extract_text`, `extract_text_auto` | correct | none — a string |
| `extract_page_text` | correct | spans, see below |
| `extract_spans` | correct | `'Figure 3'` bbox `(20.0, 100.0, 0.0, 12.0)` — **width 0** |
| `extract_words` | correct | `'Section'` bbox `(40.02, 130.0, 0.0, 12.0)` — **width 0**, and each of its seven `chars` is at x=40.02 with advance 0.00 |
| `extract_text_lines` | correct | line bbox width 0 for a line that is entirely a span |
| `extract_structured` | correct | region-level only, one `BodyBlock` for the page |
| `extract_lines` | — | returns 0 items on this file |
| `extract_chars` | **corrupted** | per-glyph, and the only per-glyph source |

So in span mode the declared characters are a zero-width point, and that point
is where the *real prefix ended* (x=40.02, the end of `"the "`) rather than
where the span starts (x=42.70). The span's own extent is not recoverable from
any of them: `extract_spans` reports a bbox that stops at the prefix, and the
suppressed second line of a multi-line span appears in nothing at all.

There is also no configuration switch. `ExtractionProfile` exposes
`tj_offset_threshold`, `word_margin_ratio`, `space_threshold_em_ratio`,
`space_char_multiplier` and `use_adaptive_threshold`; nothing about
`/ActualText`, and no way to ask character mode to leave the key alone so that
a caller can resolve it itself against the real glyphs.

For anyone hitting this before a fix lands: the replacement *is* separable from
the real text, because it forms a cursor chain. Walk from the span's origin,
take the character at the cursor, step by its own advance, and repeat for as
many characters as the declaration has UTF-8 bytes — real glyphs the
replacement overran are never at a position the cursor visits. On the minimal
file that recovers `â„¢`, `Section` and `Figure 3` exactly. It needs the byte
count to be safe: the same walk with a count of 3 instead of 7 returns `Sec`,
which is wrong and plausible. And it needs the span's origin, which means
parsing the content stream for the declarations anyway — at which point the
only thing the crate is still supplying is the geometry of the glyphs that
aren't declared.

## 6. What a fix has to decide

Not a substitution — character mode has to emit `TextChar`s for characters
that have no glyphs, so their positions are a design choice:

* **Where they go.** Distributing the declared characters across the span's
  *real* advance keeps the run inside the ink it stands for, which is what a
  consumer intersecting a rectangle needs. It also means their advances are
  synthetic, and `advance_width` no longer means "advance of this glyph".
* **How many.** Iterate characters, not bytes.
* **A span crossing a line break.** Two stretches of page, one declaration.
  Splitting the string across them invents a correspondence the key exists to
  deny; putting it all on the first line is what happens today.
* **Whether to mark them.** A flag on `TextChar` saying "this character was
  declared, not decoded" would let a consumer apply its own rule for a
  selection that covers only part of a span, which is otherwise unanswerable
  from the outside. `mcid` is already there but is `None` for an inline
  properties dictionary, which is what producers write for `/ActualText`.

Span mode's `char_widths` of `0.0` for declared characters looks like the same
question answered by declining to answer it; a fix might reasonably address
both at once.
