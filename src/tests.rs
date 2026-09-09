//! Unit tests for the geometry and text-assembly logic.
//!
//! Every test here corresponds to a bug that actually shipped during
//! development, and the comment on each says which. If one of these fails, the
//! output regressed in a way that took a manual diff against pdfannots to spot
//! the first time round.
//!
//! No PDF is needed: `TextChar` implements `Default`, so glyphs are synthesised
//! directly. `Annotation` does not implement `Default` and has ~24 fields, so
//! functions taking one (`has_quads`) are left to the integration tests.

use super::*;
use std::io::{self, Write};

// ---------------------------------------------------------------- fixtures

/// A glyph with baseline semantics, matching how pdf_oxide builds them:
/// `bbox.y == origin_y == baseline`, and `rendered_advance == width`.
fn glyph(ch: char, x: f32, baseline: f32, w: f32, h: f32) -> TextChar {
    TextChar {
        char: ch,
        bbox: Rect::new(x, baseline, w, h),
        font_size: 10.0,
        origin_x: x,
        origin_y: baseline,
        rendered_advance: w,
        ..Default::default()
    }
}

/// A glyph whose cursor advance differs from its ink width, as happens for the
/// expanded characters of an `fi`/`ff` ligature.
fn ligature_glyph(ch: char, x: f32, baseline: f32, ink_w: f32, advance: f32) -> TextChar {
    TextChar {
        rendered_advance: advance,
        ..glyph(ch, x, baseline, ink_w, 10.0)
    }
}

fn rec(page: usize, kind: &str, covered: Option<&str>, comment: Option<&str>) -> Record {
    Record {
        page,
        kind: kind.to_string(),
        covered_text: covered.map(str::to_string),
        comment: comment.map(str::to_string),
        author: None,
        modified: None,
        color: None,
        section: None,
        rect: [0.0; 4],
        link: String::new(),
        diagnostics: Vec::new(),
    }
}

/// A writer that fails every write with a chosen error kind.
struct FailingWriter(io::ErrorKind);

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(self.0, "injected"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(self.0, "injected"))
    }
}

// ------------------------------------------------- decode_pdf_string
// Pins: comments arriving as "??" because pdf_oxide runs /Contents through
// String::from_utf8_lossy with no UTF-16 handling.
//
// These assert at the call site. The encodings themselves, including the
// PDFDocEncoding table and which assumption each reading rests on, are tested
// in `pdf_string`.

#[test]
fn decodes_utf16be_with_bom() {
    let bytes = b"\xFE\xFF\x00T\x00o\x00o";
    assert_eq!(decode_pdf_string(bytes), "Too");
}

#[test]
fn decodes_utf16le_with_bom() {
    let bytes = b"\xFF\xFET\x00o\x00o\x00";
    assert_eq!(decode_pdf_string(bytes), "Too");
}

#[test]
fn decodes_utf16_surrogate_pair() {
    // U+1D44D MATHEMATICAL ITALIC CAPITAL Z, as D835 DC4D
    let bytes = b"\xFE\xFF\xD8\x35\xDC\x4D";
    assert_eq!(decode_pdf_string(bytes), "\u{1D44D}");
}

#[test]
fn decodes_ascii_unchanged() {
    assert_eq!(decode_pdf_string(b"plain comment"), "plain comment");
}

#[test]
fn decodes_pdfdoc_high_bytes_as_latin1() {
    // 0xE9 is not valid UTF-8; PDFDocEncoding matches Latin-1 here.
    assert_eq!(decode_pdf_string(b"caf\xE9"), "café");
}

#[test]
fn decodes_truncated_utf16_without_panicking() {
    // Odd trailing byte: chunks_exact drops it rather than panicking.
    assert_eq!(decode_pdf_string(b"\xFE\xFF\x00T\x00"), "T");
}

#[test]
fn decodes_empty_and_bom_only() {
    assert_eq!(decode_pdf_string(b""), "");
    assert_eq!(decode_pdf_string(b"\xFE\xFF"), "");
}

// ------------------------------------------------------ choose_text
// Pins: Okular's empty comments arriving as "??". Okular writes a bare
// UTF-16BE BOM (FE FF) for an empty comment. It decodes correctly to nothing,
// but pdf_oxide's from_utf8_lossy reading of the same two bytes is two
// replacement characters, so treating "decoded to nothing" as "key absent"
// and falling back resurrected the garbage.

#[test]
fn empty_raw_string_does_not_fall_back() {
    assert_eq!(
        choose_text(RawString::Empty, Some("\u{FFFD}\u{FFFD}")),
        None
    );
}

#[test]
fn absent_raw_string_falls_back() {
    assert_eq!(
        choose_text(RawString::Absent, Some("  a comment  ")),
        Some("a comment".to_string())
    );
    assert_eq!(choose_text(RawString::Absent, Some("   ")), None);
    assert_eq!(choose_text(RawString::Absent, None), None);
}

#[test]
fn decoded_raw_string_wins() {
    assert_eq!(
        choose_text(RawString::Text("Café".to_string()), Some("Caf?")),
        Some("Café".to_string())
    );
}

#[test]
fn bare_bom_decodes_to_nothing() {
    // The two bytes Okular actually writes.
    assert_eq!(decode_pdf_string(b"\xFE\xFF"), "");
}

// ------------------------------------------------------ to_top_down
// Pins: the one-line vertical offset. pdf_oxide constructs every glyph as
// Rect::new(origin_x, origin_y, w, h), so bbox.y is the *baseline* and the
// glyph occupies the height *above* it — despite Rect documenting y as the
// top-left corner.

#[test]
fn top_down_bottom_places_glyph_above_baseline() {
    let b = Rect::new(10.0, 100.0, 5.0, 12.0);
    let (top, bottom) = GlyphSpace::TopDownBottom.to_top_down(&b, 792.0);
    assert_eq!((top, bottom), (88.0, 100.0));
}

#[test]
fn top_down_top_places_glyph_below_y() {
    let b = Rect::new(10.0, 100.0, 5.0, 12.0);
    let (top, bottom) = GlyphSpace::TopDownTop.to_top_down(&b, 792.0);
    assert_eq!((top, bottom), (100.0, 112.0));
}

#[test]
fn bottom_up_flips_against_page_height() {
    let b = Rect::new(10.0, 100.0, 5.0, 12.0);
    let (top, bottom) = GlyphSpace::BottomUp.to_top_down(&b, 792.0);
    assert_eq!((top, bottom), (680.0, 692.0));
}

// ------------------------------------------------------------- infer
// Pins: five consecutive wrong inferences. The decisive signal is
// bbox.y == origin_y, which means y names the baseline.

#[test]
fn infers_top_down_bottom_from_baseline_semantics() {
    // 24 glyphs marching down the page, baseline semantics.
    let chars: Vec<TextChar> = (0..24)
        .map(|i| glyph('a', 50.0, 50.0 + i as f32 * 20.0, 5.0, 10.0))
        .collect();
    assert_eq!(
        GlyphSpace::infer(&chars, 792.0, false),
        Some(GlyphSpace::TopDownBottom)
    );
}

#[test]
fn infers_bottom_up_when_y_decreases_through_the_page() {
    let chars: Vec<TextChar> = (0..24)
        .map(|i| glyph('a', 50.0, 740.0 - i as f32 * 20.0, 5.0, 10.0))
        .collect();
    assert_eq!(
        GlyphSpace::infer(&chars, 792.0, false),
        Some(GlyphSpace::BottomUp)
    );
}

#[test]
fn declines_to_infer_from_too_few_glyphs() {
    // Five glyphs is not a top and a bottom to compare. Returning a
    // convention here — TopDownBottom, as this did — is a guess that reads
    // like a measurement downstream, and `page/` lost all seventeen items to
    // exactly that: no page reached the sampling threshold, so the caller
    // kept its initialiser and matched every quad in a convention pdf_oxide
    // never emits. The caller now has to choose a fallback in the open, and
    // says so on stderr when it does.
    let chars: Vec<TextChar> = (0..5)
        .map(|i| glyph('a', 50.0, 50.0 + i as f32 * 20.0, 5.0, 10.0))
        .collect();
    assert_eq!(GlyphSpace::infer(&chars, 792.0, false), None);
}

#[test]
fn infer_uses_off_page_count_when_y_is_a_real_edge() {
    // Edge semantics (origin_y != bbox.y), glyphs hugging the page bottom:
    // reading y as the top edge pushes them off the page, so bottom wins.
    let page_height = 100.0;
    let chars: Vec<TextChar> = (0..24)
        .map(|i| {
            let baseline = 60.0 + i as f32 * 1.5;
            TextChar {
                origin_y: baseline + 9.0,
                ..glyph('a', 50.0, baseline, 5.0, 12.0)
            }
        })
        .collect();
    assert_eq!(
        GlyphSpace::infer(&chars, page_height, false),
        Some(GlyphSpace::TopDownBottom)
    );

    // Same shape, glyphs hugging the page top: now the bottom reading is the
    // one that escapes, so top wins.
    let chars: Vec<TextChar> = (0..24)
        .map(|i| {
            let baseline = 0.5 + i as f32 * 0.2;
            TextChar {
                origin_y: baseline + 9.0,
                ..glyph('a', 50.0, baseline, 5.0, 12.0)
            }
        })
        .collect();
    assert_eq!(
        GlyphSpace::infer(&chars, page_height, false),
        Some(GlyphSpace::TopDownTop)
    );
}

// ------------------------------------------------------ to_page_frame
// Pins PG09 to PG13: a page whose /MediaBox corner is not the origin. Content
// stream coordinates take no notice of where the media box sits, so the glyphs
// and the quads arrive in the same absolute space — but `text_under_quads`
// subtracts the corner from every quad, and for a while nothing subtracted it
// from the glyphs. Twenty points of disagreement is two lines of body text,
// and it matched nothing at all.

#[test]
fn moves_glyphs_off_the_media_box_corner() {
    let chars = to_page_frame(vec![glyph('A', 80.0, 120.0, 6.0, 12.0)], 20.0, 20.0);
    assert_eq!(chars[0].origin_x, 60.0);
    assert_eq!(chars[0].origin_y, 100.0);
    // The bbox has to move with the origin: `covers` measures x from it.
    assert_eq!(chars[0].bbox.x, 60.0);
    assert_eq!(chars[0].bbox.y, 100.0);
}

#[test]
fn leaves_glyphs_alone_at_the_origin() {
    let chars = to_page_frame(vec![glyph('A', 80.0, 120.0, 6.0, 12.0)], 0.0, 0.0);
    assert_eq!((chars[0].origin_x, chars[0].origin_y), (80.0, 120.0));
}

// ------------------------------------------------------------ covers
// Band under test: top-down 88..100 (one 12pt line), x 50..150.

const QTOP: f32 = 88.0;
const QBOTTOM: f32 = 100.0;
const QX0: f32 = 50.0;
const QX1: f32 = 150.0;

fn covers_default(c: &TextChar, gtop: f32, gbottom: f32) -> bool {
    covers(c, gtop, gbottom, QX0, QX1, QTOP, QBOTTOM, 0.5)
}

#[test]
fn covers_glyph_inside_the_band() {
    let g = glyph('A', 60.0, 100.0, 6.0, 12.0);
    assert!(covers_default(&g, 88.0, 100.0));
}

#[test]
fn rejects_glyph_on_the_next_line() {
    // Pins: adjacent-line contamination ("we review ta commonly").
    let g = glyph('B', 60.0, 112.0, 6.0, 12.0);
    assert!(!covers_default(&g, 100.0, 112.0));
}

#[test]
fn accepts_zero_height_synthetic_space() {
    // Pins: "scale?and". Space glyphs have no height, so area overlap can
    // never admit them; only the centre test can.
    let g = glyph(' ', 100.0, 94.0, 0.0, 0.0);
    assert!(covers_default(&g, 94.0, 94.0));
    // Confirm why: area overlap alone rejects it.
    assert_eq!(
        overlap_fraction(100.0, 100.0, 94.0, 94.0, QX0, QX1, QTOP, QBOTTOM),
        0.0
    );
}

#[test]
fn accepts_the_final_glyph_of_a_selection() {
    // The last character of a selection sits inside the quad, so no horizontal
    // padding is needed to catch it. (An earlier version of this test asserted
    // a half-glyph pad, on the theory that "leverage effec" was clipped by a
    // quad stopping short. It was not: the vertical geometry was wrong, and the
    // pad was a workaround for that misdiagnosis.)
    let g = glyph('s', 143.0, 100.0, 6.0, 12.0); // midpoint 146, inside 50..150
    assert!(covers_default(&g, 88.0, 100.0));
}

#[test]
fn rejects_a_trailing_period_outside_the_quad() {
    // Pins E5A: "encoded comment [[E5A]]." picked up the full stop while the
    // identical construct in E1C did not. The old half-glyph pad was the same
    // order as the overshoot, so the two cases fell on opposite sides of a
    // knife edge. The midpoint is outside the quad, so it must be excluded.
    let g = glyph('.', 150.2, 100.0, 2.7, 3.0); // midpoint 151.55
    assert!(!covers_default(&g, 97.0, 100.0));
}

#[test]
fn rejects_glyph_far_beyond_the_quad() {
    let g = glyph('z', 200.0, 100.0, 6.0, 12.0);
    assert!(!covers_default(&g, 88.0, 100.0));
}

#[test]
fn admits_a_large_operator_spanning_the_band() {
    // Pins E3C: a summation glyph is ~44pt tall, so its centre sits well above
    // a 12pt line band and the centre test rejects it. What qualifies it is
    // spanning the whole band at that x position.
    let g = glyph('\u{2211}', 60.0, 104.0, 6.0, 44.0);
    assert!(covers(&g, 60.0, 104.0, QX0, QX1, QTOP, QBOTTOM, 0.5));
    // Band coverage governs this case, not min_overlap: a high threshold no
    // longer excludes a large operator.
    assert!(covers(&g, 60.0, 104.0, QX0, QX1, QTOP, QBOTTOM, 0.95));
}

#[test]
fn band_coverage_gate_excludes_merely_taller_glyphs() {
    // The large-operator rule is gated on being more than 1.3x the band. A
    // 14pt glyph against a 12pt band is 1.17x, so it is judged by its centre
    // like any other glyph — and its centre is on the following line. Lowering
    // that gate would let neighbouring lines back in wholesale.
    let g = glyph('B', 60.0, 114.0, 6.0, 14.0);
    assert!(!covers_default(&g, 100.0, 114.0));
}

// ----------------------------------------------------- overlap_fraction

#[test]
fn overlap_fraction_measures_the_glyph_not_the_quad() {
    // Glyph 10 wide, 12 tall, half inside horizontally, fully inside vertically.
    let f = overlap_fraction(45.0, 55.0, 88.0, 100.0, QX0, QX1, QTOP, QBOTTOM);
    assert!((f - 0.5).abs() < 1e-6, "expected 0.5, got {f}");
}

#[test]
fn overlap_fraction_is_zero_when_disjoint() {
    assert_eq!(
        overlap_fraction(200.0, 210.0, 88.0, 100.0, QX0, QX1, QTOP, QBOTTOM),
        0.0
    );
}

// ---------------------------------------------------- order_reading

const SPACE: GlyphSpace = GlyphSpace::TopDownBottom;
const H: f32 = 792.0;

#[test]
fn orders_glyphs_left_to_right_regardless_of_input_order() {
    // Pins: the whole selection coming out reversed, from sorting on origin_y
    // in the wrong direction.
    let a = glyph('a', 10.0, 100.0, 6.0, 10.0);
    let b = glyph('b', 16.0, 100.0, 6.0, 10.0);
    let c = glyph('c', 22.0, 100.0, 6.0, 10.0);
    let hits = vec![&c, &b, &a];
    assert_eq!(order_reading(&hits, SPACE, H, 0.25), "abc");
}

#[test]
fn orders_lines_down_the_page_and_joins_with_a_space() {
    let a = glyph('a', 10.0, 100.0, 6.0, 10.0);
    let b = glyph('b', 10.0, 112.0, 6.0, 10.0);
    // Lower line supplied first.
    let hits = vec![&b, &a];
    assert_eq!(order_reading(&hits, SPACE, H, 0.25), "a b");
}

#[test]
fn inserts_a_space_across_a_wide_gap() {
    // Pins: "location?" — typeset maths emits no space glyph, so the space has
    // to come from the gap. font_size 10 * 0.25 = 2.5pt threshold.
    let a = glyph('a', 10.0, 100.0, 6.0, 10.0);
    let b = glyph('b', 20.0, 100.0, 6.0, 10.0);
    let hits = vec![&a, &b];
    assert_eq!(order_reading(&hits, SPACE, H, 0.25), "a b");
}

#[test]
fn does_not_insert_a_space_after_a_ligature() {
    // Pins: "short-termfi xed" and "leverage eff ec". For ligature-expanded
    // characters bbox.width is the ink width divided by the character count,
    // so measuring the gap from bbox underestimates the cursor and fabricates
    // a break. rendered_advance is the correct measure.
    let f = ligature_glyph('f', 10.0, 100.0, 1.0, 4.0);
    let i = ligature_glyph('i', 14.0, 100.0, 1.0, 4.0);
    let x = glyph('x', 18.0, 100.0, 6.0, 10.0);
    let hits = vec![&f, &i, &x];
    assert_eq!(order_reading(&hits, SPACE, H, 0.25), "fix");
}

#[test]
fn keeps_a_subscript_on_its_own_line() {
    // Pins: "Kvalid" splitting apart. The subscript baseline is 3pt below, well
    // within the 0.6 * median-height tolerance.
    let k = glyph('K', 10.0, 100.0, 7.0, 10.0);
    let v = glyph('v', 17.0, 103.0, 5.0, 7.0);
    let hits = vec![&k, &v];
    assert_eq!(order_reading(&hits, SPACE, H, 0.25), "Kv");
}

#[test]
fn order_reading_handles_no_hits() {
    assert_eq!(order_reading(&[], SPACE, H, 0.25), "");
}

// -------------------------------------------------- text_under_quads
// Page height 200, MediaBox origin at (0, 0). Quads are in y-up user space;
// a band covering top-down 88..100 is user-space y 100..112.

fn quad(x0: f64, x1: f64, uy0: f64, uy1: f64) -> [f64; 8] {
    // Deliberately not in a tidy corner order: producers disagree, and the
    // implementation must take the extent over all four points.
    [x1, uy0, x0, uy1, x1, uy1, x0, uy0]
}

#[test]
fn extracts_only_the_glyphs_under_the_quad() {
    let line_a = glyph('A', 60.0, 100.0, 6.0, 12.0);
    let line_b = glyph('B', 60.0, 112.0, 6.0, 12.0);
    let chars = vec![line_a, line_b];
    let quads = vec![quad(50.0, 150.0, 100.0, 112.0)];

    let out = quad_texts(&quads, &chars, SPACE, 0.0, 0.0, 200.0, 0.5, 0.25, false);
    assert_eq!(out, vec!["A".to_string()]);
}

#[test]
fn sorts_quads_into_reading_order() {
    // Pins: p.3 coming out with its second line first. Some producers store
    // /QuadPoints bottom-up, so array order cannot be trusted.
    let line_a = glyph('A', 60.0, 100.0, 6.0, 12.0);
    let line_b = glyph('B', 60.0, 112.0, 6.0, 12.0);
    let chars = vec![line_a, line_b];
    let quads = vec![
        quad(50.0, 150.0, 88.0, 100.0),  // lower line, listed first
        quad(50.0, 150.0, 100.0, 112.0), // upper line
    ];

    let out = quad_texts(&quads, &chars, SPACE, 0.0, 0.0, 200.0, 0.5, 0.25, false);
    assert_eq!(out, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn empty_quad_yields_an_empty_string_not_a_dropped_entry() {
    // The caller counts these to warn about geometry problems, so they must
    // survive as empty entries rather than vanishing.
    let chars = vec![glyph('A', 60.0, 100.0, 6.0, 12.0)];
    let quads = vec![quad(400.0, 500.0, 100.0, 112.0)];

    let out = quad_texts(&quads, &chars, SPACE, 0.0, 0.0, 200.0, 0.5, 0.25, false);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0], "");
}

#[test]
fn a_glyph_is_claimed_by_only_one_quad() {
    // Pins E3C's duplicated 'i'. Two vertically overlapping bands both satisfy
    // the centre test for the same glyph — a stacked limit sitting inside the
    // main line's slack. It must be emitted once, by the upper band, which
    // comes first in reading order.
    let g = glyph('i', 60.0, 100.0, 6.0, 12.0);
    let chars = vec![g];
    let quads = vec![
        quad(50.0, 150.0, 100.0, 112.0), // top-down 88..100, contains the glyph
        quad(50.0, 150.0, 97.0, 109.0),  // top-down 91..103, also contains it
    ];

    let out = quad_texts(&quads, &chars, SPACE, 0.0, 0.0, 200.0, 0.5, 0.25, false);
    // The two bands overlap vertically, so they form one line group; the glyph
    // appears once in it rather than twice.
    assert_eq!(out, vec!["i".to_string()]);
}

#[test]
fn merges_per_glyph_quads_on_one_line() {
    // Pins E8A: Papers emitted 37 quads for a 34-character selection on the
    // rotated page, roughly one per glyph. Treating each as its own line put a
    // space between every character ("A highlight s its on a rotated p age").
    // Bands that overlap vertically belong to one line.
    let a = glyph('a', 10.0, 100.0, 6.0, 12.0);
    let b = glyph('b', 16.0, 100.0, 6.0, 12.0);
    let c = glyph('c', 22.0, 100.0, 6.0, 12.0);
    let chars = vec![a, b, c];
    let quads = vec![
        quad(9.0, 16.0, 100.0, 112.0),
        quad(15.0, 22.0, 100.0, 112.0),
        quad(21.0, 29.0, 100.0, 112.0),
    ];

    let out = quad_texts(&quads, &chars, SPACE, 0.0, 0.0, 200.0, 0.5, 0.25, false);
    assert_eq!(out, vec!["abc".to_string()]);
}

#[test]
fn honours_a_non_zero_mediabox_origin() {
    // Glyph coordinates are relative to the page origin; quad coordinates are
    // absolute, so the MediaBox corner has to be subtracted.
    let chars = vec![glyph('A', 60.0, 100.0, 6.0, 12.0)];
    let quads = vec![quad(70.0, 170.0, 120.0, 132.0)];

    let out = quad_texts(&quads, &chars, SPACE, 20.0, 20.0, 200.0, 0.5, 0.25, false);
    assert_eq!(out, vec!["A".to_string()]);
}

// --------------------------------------------------- ligature folding
// Folding is on by default and confined to U+FB00–U+FB06. It runs on glyph
// text on its way into a line — after quad matching, so a ligature is still
// atomic for coverage, and after /ActualText, so a declaration is never undone.

fn recovered(chars: &[TextChar], quads: &[[f64; 8]], fold: bool) -> Vec<String> {
    text_under_quads(
        quads,
        chars,
        &Matching {
            space: SPACE,
            frame: Frame {
                x0: 0.0,
                y0: 0.0,
                height: 200.0,
            },
            min_overlap: 0.5,
            space_gap: 0.25,
            fold_ligatures: fold,
            debug: false,
            units: &[],
        },
    )
    .into_iter()
    .map(|l| l.text)
    .collect()
}

#[test]
fn folds_a_presentation_form_to_its_letters() {
    // Pins text/ TX1A. The font maps its ff glyph to U+FB00 through
    // /ToUnicode, and a note reading `tariﬀ` cannot be searched for or pasted
    // anywhere useful.
    let chars = vec![glyph('\u{FB00}', 60.0, 100.0, 6.0, 12.0)];
    let quads = vec![quad(50.0, 150.0, 100.0, 112.0)];
    assert_eq!(recovered(&chars, &quads, true), vec!["ff"]);
}

#[test]
fn keep_ligatures_leaves_the_glyph_as_the_font_maps_it() {
    let chars = vec![glyph('\u{FB00}', 60.0, 100.0, 6.0, 12.0)];
    let quads = vec![quad(50.0, 150.0, 100.0, 112.0)];
    assert_eq!(recovered(&chars, &quads, false), vec!["\u{FB00}"]);
}

#[test]
fn folding_does_not_touch_letters_that_were_once_ligatures() {
    // Pins text/ TX6A, and it passed before folding existed only because
    // nothing was folded at all. Æ is a letter of Danish, œ of French — and
    // Unicode names it LATIN SMALL LIGATURE OE, which is the trap — and ß of
    // German. cli-pdf-extract folds Æ to `fl`.
    let chars = vec![
        glyph('Æ', 60.0, 100.0, 6.0, 12.0),
        glyph('œ', 66.0, 100.0, 6.0, 12.0),
        glyph('ß', 72.0, 100.0, 6.0, 12.0),
    ];
    let quads = vec![quad(50.0, 150.0, 100.0, 112.0)];
    assert_eq!(recovered(&chars, &quads, true), vec!["Æœß"]);
}

#[test]
fn folding_divides_the_advance_between_the_letters() {
    // The cursor after a folded glyph has to land where the glyph's own
    // advance ended, or the gap to the next character is mismeasured and
    // `staﬀ on` comes out as `sta f f on`. Threshold here is 10 * 0.25.
    let chars = vec![
        glyph('\u{FB00}', 60.0, 100.0, 6.0, 12.0),
        glyph('x', 66.0, 100.0, 6.0, 12.0),
    ];
    let quads = vec![quad(50.0, 150.0, 100.0, 112.0)];
    assert_eq!(recovered(&chars, &quads, true), vec!["ffx"]);
}

// ------------------------------------------------- declared spans
// A declared span is atomic: a quad covering part of one cannot be told which
// part of the declared text corresponds, so it is emitted whole above the
// threshold and omitted entirely below it, and once however many quads touch
// it. These use BottomUp, unlike the tests above, because that is the
// convention the declarations share with pdf_oxide's glyphs: `Frame` measures
// a segment baseline upward from the media box corner.

/// A declaration covering one line.
fn declared(text: &str, x: f32, baseline: f32, advance: f32) -> Unit {
    Unit {
        text: text.to_string(),
        segments: vec![actual_text::Segment {
            x,
            baseline,
            advance,
        }],
    }
}

fn page_frame() -> Frame {
    Frame {
        x0: 0.0,
        y0: 0.0,
        height: 200.0,
    }
}

fn declared_lines(quads: &[[f64; 8]], chars: &[TextChar], units: &[Unit]) -> Vec<String> {
    text_under_quads(
        quads,
        chars,
        &Matching {
            space: GlyphSpace::BottomUp,
            frame: page_frame(),
            min_overlap: 0.5,
            space_gap: 0.25,
            fold_ligatures: true,
            debug: false,
            units,
        },
    )
    .into_iter()
    .map(|l| l.text)
    .collect()
}

#[test]
fn a_majority_covered_declaration_is_emitted_whole() {
    // Pins AT5A. The quad stops inside the span, three quarters of the way
    // along, and the answer is the whole declared string — not the part of it
    // the quad reached, which would be a guess about correspondence the
    // producer has said does not exist.
    let chars = vec![glyph('A', 50.0, 100.0, 6.0, 12.0)];
    let quads = vec![quad(50.0, 130.0, 100.0, 112.0)];
    let units = vec![declared("Figure 3", 100.0, 100.0, 40.0)];

    assert_eq!(declared_lines(&quads, &chars, &units), vec!["A Figure 3"]);
}

#[test]
fn a_minority_covered_declaration_is_omitted_entirely() {
    // Pins AT5B, the mirror. A quarter covered, so nothing of the declaration
    // appears — and in particular not the glyphs underneath it, which is the
    // tempting third option and is wrong.
    let chars = vec![glyph('A', 50.0, 100.0, 6.0, 12.0)];
    let quads = vec![quad(50.0, 110.0, 100.0, 112.0)];
    let units = vec![declared("Figure 3", 100.0, 100.0, 40.0)];

    assert_eq!(declared_lines(&quads, &chars, &units), vec!["A"]);
}

#[test]
fn a_declaration_across_two_quads_is_emitted_once() {
    // Pins AT7A's first failure mode: one declaration broken across a line
    // break is still one declaration, so applying it per quad says the same
    // words twice.
    let chars = vec![
        glyph('A', 50.0, 100.0, 6.0, 12.0), // upper line
        glyph('B', 10.0, 88.0, 6.0, 12.0),  // lower line
    ];
    let quads = vec![
        quad(50.0, 140.0, 100.0, 112.0), // upper line, over the first segment
        quad(5.0, 60.0, 88.0, 100.0),    // lower line, over the second
    ];
    let units = vec![Unit {
        text: "Figure 3".to_string(),
        segments: vec![
            actual_text::Segment {
                x: 100.0,
                baseline: 100.0,
                advance: 40.0,
            },
            actual_text::Segment {
                x: 10.0,
                baseline: 88.0,
                advance: 20.0,
            },
        ],
    }];

    let out = declared_lines(&quads, &chars, &units);
    assert_eq!(out, vec!["A Figure 3", "B"]);
    assert_eq!(
        out.iter().filter(|l| l.contains("Figure 3")).count(),
        1,
        "the declared text appeared more than once"
    );
}

#[test]
fn a_declared_span_is_never_folded() {
    // Pins AT2A: the document declares U+FB01, so the default fold must not
    // undo it. Folding is a text-recovery transform, and declared text was
    // not recovered from anything — the producer stated it. The two are only
    // told apart by running the family both ways: under --keep-ligatures,
    // AT1A stays `tariffs` because the declaration is authoritative while
    // AT1B becomes `tariﬀs` because the fold is off.
    let chars = vec![glyph('A', 50.0, 100.0, 6.0, 12.0)];
    let quads = vec![quad(50.0, 130.0, 100.0, 112.0)];
    let units = vec![declared("\u{FB01}xed", 100.0, 100.0, 40.0)];

    assert_eq!(
        declared_lines(&quads, &chars, &units),
        vec!["A \u{FB01}xed"]
    );
}

#[test]
fn synthesised_characters_span_the_declared_extent() {
    // The characters are spread across the extent of the glyphs they replace,
    // which is safe only because the decision to emit was taken for the unit
    // as a whole: they are never matched individually.
    let template = glyph('x', 0.0, 100.0, 6.0, 10.0);
    let out = actual_text::synthesise(
        &template,
        "abcd",
        actual_text::Segment {
            x: 100.0,
            baseline: 100.0,
            advance: 40.0,
        },
        page_frame(),
    );
    assert_eq!(out.len(), 4);
    assert_eq!(out[0].origin_x, 100.0);
    assert_eq!(out[3].origin_x, 130.0);
    assert_eq!(out[0].rendered_advance, 10.0);
    // The baseline comes from the segment, not the template, so a template
    // borrowed from another line cannot drag the text off its own.
    assert_eq!(out[0].origin_y, 100.0);
}

// -------------------------------------------------------- join_lines

#[test]
fn an_empty_declaration_joins_the_two_sides_contiguously() {
    // Pins AT4A. The hyphen carries /ActualText (): the document says that
    // glyph stands for no character. The two sides are therefore contiguous —
    // no space and no hyphen — and the same under --keep-hyphens, because
    // there is nothing left to guess. Contrast text/ TX4, where the same break
    // in an untagged PDF is genuinely ambiguous and no setting is right for
    // both cases.
    //
    // Note the coverage is zero: the suppressed hyphen sits at x=116..120 and
    // the quad on that line ends at 120, so the producer selected up to the
    // glyph, not through it. What makes the join contiguous is that the thing
    // standing for nothing is adjacent to the break.
    let lines = vec![
        Line {
            text: "infra".to_string(),
            band: (88.0, 100.0),
            right: Some(120.0),
        },
        Line {
            text: "structure".to_string(),
            band: (100.0, 112.0),
            right: Some(80.0),
        },
    ];
    let units = vec![declared("", 116.0, 100.0, 4.0)];

    assert_eq!(
        join_lines(&lines, false, &units, page_frame()),
        "infrastructure"
    );
    assert_eq!(
        join_lines(&lines, true, &units, page_frame()),
        "infrastructure"
    );
    // Without the declaration the same two lines are just two lines.
    assert_eq!(
        join_lines(&lines, false, &[], page_frame()),
        "infra structure"
    );
}

#[test]
fn joins_hyphenated_words_across_lines() {
    let lines = vec!["soft-".to_string(), "ware".to_string()];
    assert_eq!(join_plain(&lines, false), "software");
}

#[test]
fn keep_hyphens_preserves_the_break() {
    let lines = vec!["soft-".to_string(), "ware".to_string()];
    assert_eq!(join_plain(&lines, true), "soft- ware");
}

#[test]
fn does_not_join_when_the_next_line_starts_uppercase() {
    // "Basel-" + "Committee" is a real hyphen, not a line break.
    let lines = vec!["Basel-".to_string(), "Committee".to_string()];
    assert_eq!(join_plain(&lines, false), "Basel- Committee");
}

#[test]
fn separates_plain_lines_with_one_space() {
    let lines = vec!["first".to_string(), "second".to_string()];
    assert_eq!(join_plain(&lines, false), "first second");
}

// ---------------------------------------------------- collapse_spaces

#[test]
fn collapses_runs_of_whitespace() {
    assert_eq!(collapse_spaces("a  \t\n b"), "a b");
}

#[test]
fn leaves_single_spaces_alone() {
    assert_eq!(collapse_spaces("a b c"), "a b c");
}

// --------------------------------------------------------- hex_color

#[test]
fn converts_device_rgb() {
    assert_eq!(hex_color(&[1.0, 0.0, 0.0]), Some("#ff0000".to_string()));
}

#[test]
fn converts_device_gray() {
    assert_eq!(hex_color(&[0.5]), Some("#808080".to_string()));
}

#[test]
fn converts_device_cmyk() {
    assert_eq!(
        hex_color(&[0.0, 1.0, 1.0, 0.0]),
        Some("#ff0000".to_string())
    );
    assert_eq!(
        hex_color(&[0.0, 0.0, 0.0, 1.0]),
        Some("#000000".to_string())
    );
}

#[test]
fn rejects_unknown_colour_arity() {
    assert_eq!(hex_color(&[]), None);
    assert_eq!(hex_color(&[0.1, 0.2]), None);
}

#[test]
fn clamps_out_of_range_components() {
    assert_eq!(hex_color(&[2.0, -1.0, 0.0]), Some("#ff0000".to_string()));
}

// -------------------------------------------------- subtype handling

#[test]
fn markup_and_note_subtypes_are_interesting() {
    assert!(is_interesting(&AnnotationSubtype::Highlight));
    assert!(is_interesting(&AnnotationSubtype::Underline));
    assert!(is_interesting(&AnnotationSubtype::Text));
    assert!(is_interesting(&AnnotationSubtype::FreeText));
}

#[test]
fn structural_subtypes_are_not_interesting() {
    assert!(!is_interesting(&AnnotationSubtype::Popup));
    assert!(!is_interesting(&AnnotationSubtype::Link));
    assert!(!is_interesting(&AnnotationSubtype::Widget));
}

// ------------------------------------------------------ sort_records
// Pins: output following /Annots creation order instead of reading order.

#[test]
fn sorts_by_page_then_down_then_across() {
    let mut records = vec![
        {
            let mut r = rec(2, "note", None, Some("second page"));
            r.rect = [100.0, 700.0, 120.0, 712.0];
            r
        },
        {
            let mut r = rec(1, "highlight", None, Some("lower left"));
            r.rect = [50.0, 200.0, 90.0, 212.0];
            r
        },
        {
            let mut r = rec(1, "highlight", None, Some("upper right"));
            r.rect = [300.0, 700.0, 340.0, 712.0];
            r
        },
        {
            let mut r = rec(1, "highlight", None, Some("upper left"));
            r.rect = [50.0, 700.0, 90.0, 712.0];
            r
        },
    ];

    sort_records(&mut records);

    let order: Vec<&str> = records
        .iter()
        .map(|r| r.comment.as_deref().unwrap())
        .collect();
    assert_eq!(
        order,
        vec!["upper left", "upper right", "lower left", "second page"]
    );
}

// -------------------------------------------------------- write_to
// Pins the broken-pipe branch. `println!` panics when stdout is gone, which
// is what `tool paper.pdf | head` does. It went unnoticed because a single
// write below the 64 KiB pipe buffer completes before the reader exits;
// measured, it aborts with exit 101 from about 70 KB up.

#[test]
fn writes_the_whole_report_to_the_sink() {
    let mut sink: Vec<u8> = Vec::new();
    assert!(write_to(&mut sink, "hello").is_ok());
    assert_eq!(sink, b"hello");
}

#[test]
fn broken_pipe_is_not_an_error() {
    // `| head` closing the pipe early is normal for a filter, so exit 0.
    assert!(write_to(FailingWriter(io::ErrorKind::BrokenPipe), "x").is_ok());
}

#[test]
fn other_write_errors_still_propagate() {
    // A full disk or a bad redirect must not be swallowed along with it.
    assert!(write_to(FailingWriter(io::ErrorKind::PermissionDenied), "x").is_err());
}

// -------------------------------------------------- repair, broken chain
// The branch that had never run in a test. `repair` finds the replacement by
// walking a chain of characters at successive cursor positions; when that
// chain is not there whole, the declaration is dropped and the corrupted
// glyphs are LEFT IN THE LIST. Nothing removes them, so the quads go on to
// match them and no geometric warning fires. These pin that, because the
// behaviour is only defensible if it is reported, and it is only reported if
// the diagnostic reaches the items affected.

/// The three characters pdf_oxide typesets in place of a declared `™`: its
/// UTF-8 bytes pushed back through the font, at successive cursor positions.
fn mojibake_chain(x: f32, baseline: f32) -> Vec<TextChar> {
    ['â', '„', '¢']
        .iter()
        .enumerate()
        .map(|(i, &c)| glyph(c, x + i as f32 * 3.0, baseline, 3.0, 10.0))
        .collect()
}

fn declaration(text: &str, x: f32, baseline: f32, advance: f32) -> actual_text::Declaration {
    actual_text::Declaration {
        text: text.to_string(),
        segments: vec![actual_text::Segment {
            x,
            baseline,
            advance,
        }],
    }
}

#[test]
fn a_complete_chain_is_removed_and_becomes_a_unit() {
    // The control for the two tests below: when the chain is all there, the
    // corrupted glyphs go and the declaration is handed back as a unit.
    let mut chars = mojibake_chain(100.0, 700.0);
    let decls = vec![declaration("\u{2122}", 100.0, 700.0, 9.0)];
    let (units, broken) = actual_text::repair(&mut chars, &decls, 1);
    assert_eq!(units.len(), 1);
    assert_eq!(units[0].text, "\u{2122}");
    assert!(broken.is_empty());
    assert!(chars.is_empty(), "the replacement should have been removed");
}

#[test]
fn a_broken_chain_leaves_the_mojibake_in_the_glyph_list() {
    // One character short of the chain, as happens when the run is interrupted
    // — a line break, or a real glyph the walk cannot step past. `wanted` is 3
    // because `™` is three UTF-8 bytes.
    let mut chars = mojibake_chain(100.0, 700.0);
    chars.pop();
    let decls = vec![declaration("\u{2122}", 100.0, 700.0, 9.0)];

    let (units, broken) = actual_text::repair(&mut chars, &decls, 1);

    assert!(units.is_empty(), "a guess must not be emitted as a unit");
    // TextChar has no PartialEq, and the characters are the point anyway:
    // this is the mojibake that goes on to reach the reader's notes.
    assert_eq!(
        chars.iter().map(|c| c.char).collect::<String>(),
        "\u{e2}\u{201e}",
        "the corrupted glyphs must be left in place"
    );
    assert_eq!(broken.len(), 1);
    assert_eq!(
        broken[0].diagnostic,
        Diagnostic::DeclarationUnrepaired {
            page: 1,
            text: "\u{2122}".to_string(),
            found: 2,
            wanted: 3,
            x: 100.0,
        }
    );
}

#[test]
fn a_broken_chain_reports_the_extent_so_it_can_be_attributed() {
    // The segments are the whole point of returning `Unrepaired` rather than a
    // bare diagnostic: without them the warning can only be said of the page.
    let mut chars = mojibake_chain(100.0, 700.0);
    chars.pop();
    let decls = vec![declaration("\u{2122}", 100.0, 700.0, 9.0)];
    let (_, broken) = actual_text::repair(&mut chars, &decls, 1);
    assert_eq!(broken[0].segments.len(), 1);
    assert_eq!(broken[0].segments[0].x, 100.0);
    assert_eq!(broken[0].segments[0].baseline, 700.0);
    assert_eq!(broken[0].segments[0].advance, 9.0);
}

// ------------------------------------------- quads_reach_segments
// Which annotations a broken declaration is reported against. Raw user space
// on both sides, y-up: no frame, no flip.

/// A rectangular quad, given as the eight floats /QuadPoints holds.
fn quad2(x0: f64, y0: f64, x1: f64, y1: f64) -> [f64; 8] {
    [x0, y1, x1, y1, x0, y0, x1, y0]
}

fn seg(x: f32, baseline: f32, advance: f32) -> actual_text::Segment {
    actual_text::Segment {
        x,
        baseline,
        advance,
    }
}

#[test]
fn a_quad_over_the_declaration_reaches_it() {
    let quads = [quad2(90.0, 695.0, 200.0, 710.0)];
    assert!(quads_reach_segments(&quads, &[seg(100.0, 700.0, 9.0)]));
}

#[test]
fn a_quad_on_another_line_does_not() {
    // The vertical test is the baseline, which is what keeps a highlight on
    // line N from being blamed for a declaration on line N+1.
    let quads = [quad2(90.0, 675.0, 200.0, 690.0)];
    assert!(!quads_reach_segments(&quads, &[seg(100.0, 700.0, 9.0)]));
}

#[test]
fn a_quad_left_of_the_declaration_does_not() {
    let quads = [quad2(10.0, 695.0, 99.0, 710.0)];
    assert!(!quads_reach_segments(&quads, &[seg(100.0, 700.0, 9.0)]));
}

#[test]
fn touching_at_an_edge_counts_as_reaching() {
    // Inclusive on purpose. A selection that ends exactly where the
    // declaration begins may or may not have taken its first glyph, and the
    // cost of guessing wrong is asymmetric: a warning on an item that turns
    // out fine is noise, a missing one is a wrong quotation.
    let quads = [quad2(10.0, 695.0, 100.0, 710.0)];
    assert!(quads_reach_segments(&quads, &[seg(100.0, 700.0, 9.0)]));
}

#[test]
fn any_quad_reaching_any_segment_is_enough() {
    // A declaration broken across a line break has one segment per line, and a
    // multi-line selection has one quad per line. Either side matching once is
    // enough to make the item suspect.
    let quads = [
        quad2(10.0, 675.0, 80.0, 690.0),
        quad2(90.0, 695.0, 200.0, 710.0),
    ];
    let segments = [seg(300.0, 720.0, 9.0), seg(100.0, 700.0, 9.0)];
    assert!(quads_reach_segments(&quads, &segments));
}

#[test]
fn a_declaration_with_no_extent_reaches_nothing() {
    // `DeclarationUnanchored` carries no segments, so it can never be
    // attributed to an item and stays on the document channel.
    let quads = [quad2(0.0, 0.0, 1000.0, 1000.0)];
    assert!(!quads_reach_segments(&quads, &[]));
}

#[test]
fn corner_order_within_a_quad_does_not_matter() {
    // Producers disagree on /QuadPoints corner order, so the extent is taken
    // over all four points. Same rectangle, corners rotated.
    let rotated = [200.0, 710.0, 90.0, 710.0, 200.0, 695.0, 90.0, 695.0];
    assert!(quads_reach_segments(&[rotated], &[seg(100.0, 700.0, 9.0)]));
}

// ------------------------------------------------------------ Options
// Pins the wiring between the CLI and `extract`. Every flag here has to reach
// `Options` or it is parsed and discarded — which is not hypothetical: --show
// and --number were both accepted and ignored for a while, and nothing failed
// because nothing checked.

#[test]
fn defaults_match_the_clap_defaults() {
    // Two sources of truth for the same numbers — the clap attribute and
    // Options::default — so they have to be compared somewhere.
    let from_cli = Options::from(&Args::parse_from(["x", "paper.pdf"]));
    let plain = Options::default();
    assert_eq!(from_cli.min_overlap, plain.min_overlap);
    assert_eq!(from_cli.space_gap, plain.space_gap);
    assert_eq!(from_cli.geometry, plain.geometry);
    assert!(!from_cli.keep_hyphens);
    assert!(!from_cli.keep_ligatures);
    assert!(!from_cli.split_quads);
    assert!(!from_cli.keep_empty);
    assert!(!from_cli.page_counts);
}

#[test]
fn auto_geometry_is_the_absence_of_a_choice() {
    // `GeometryArg::Auto` is a CLI affordance; the library says `None`, so
    // "measure it" and "assume this" cannot be confused for one another.
    assert_eq!(
        Options::from(&Args::parse_from(["x", "p.pdf"])).geometry,
        None
    );
    assert_eq!(
        Options::from(&Args::parse_from(["x", "p.pdf", "--geometry", "bottom-up"])).geometry,
        Some(GlyphSpace::BottomUp)
    );
}

#[test]
fn every_extraction_flag_reaches_options() {
    let opts = Options::from(&Args::parse_from([
        "x",
        "paper.pdf",
        "--min-overlap",
        "0.7",
        "--space-gap",
        "0.4",
        "--geometry",
        "top-down-top",
        "--keep-hyphens",
        "--keep-ligatures",
        "--split-quads",
        "--keep-empty",
        "--stats",
        "--debug-geometry",
        "--debug-quads",
    ]));
    assert_eq!(opts.min_overlap, 0.7);
    assert_eq!(opts.space_gap, 0.4);
    assert_eq!(opts.geometry, Some(GlyphSpace::TopDownTop));
    assert!(opts.keep_hyphens);
    assert!(opts.keep_ligatures);
    assert!(opts.split_quads);
    assert!(opts.keep_empty);
    assert!(opts.page_counts);
    assert!(opts.debug_geometry);
    assert!(opts.debug_quads);
}

#[test]
fn the_document_name_comes_from_the_path() {
    // `extract` takes bytes and has no filename, so the `link` fragment is
    // only as good as what the caller passes.
    let opts = Options::from(&Args::parse_from(["x", "/home/rai/papers/merton.pdf"]));
    assert_eq!(opts.document_name.as_deref(), Some("merton.pdf"));
}
