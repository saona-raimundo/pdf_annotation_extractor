//! The glyph coordinate convention, and moving glyphs into the page frame.
//!
//! pdf_oxide does not document which convention it emits, and its own `Rect`
//! documentation contradicts what it produces, so the convention is measured
//! from the glyphs rather than trusted. Everything here exists because that
//! measurement was wrong five times running.

use pdf_oxide::annotations::Annotation;
use pdf_oxide::geometry::Rect;
use pdf_oxide::layout::TextChar;
use serde::Serialize;

use crate::document::has_quads;

#[derive(Copy, Clone, Debug, PartialEq, Serialize)]
pub enum GlyphSpace {
    TopDownTop,
    TopDownBottom,
    BottomUp,
}

impl GlyphSpace {
    /// The convention to assume when there is nothing to infer from.
    ///
    /// pdf_oxide builds every glyph from the text matrix in unrotated user
    /// space, which is y-up, so this is the only convention it ever produces.
    /// The others exist because the crate's own `Rect` documentation describes
    /// a top-down box and a future version may deliver one; they are not
    /// candidates for a document we know nothing about.
    pub(crate) const FALLBACK: GlyphSpace = GlyphSpace::BottomUp;

    /// Infer the `bbox` convention from the glyphs on a page.
    ///
    /// pdf_oxide builds every glyph as `Rect::new(origin_x, origin_y, w, h)`,
    /// so `bbox.y` *is* the baseline, not an edge of the box — the "top-left
    /// corner" doc comment on `geometry::Rect` does not hold here. When we
    /// detect that (`bbox.y == origin_y`), the glyph occupies the height above
    /// the baseline, which in a top-down space means `[y - h, y]`.
    ///
    /// Extraction order runs roughly top-to-bottom in a normal document, so
    /// comparing the first glyphs' y against the last glyphs' y tells us which
    /// way y grows.
    ///
    /// `None` when the page carries too few glyphs to compare a top against a
    /// bottom. Returning a convention there would be a guess wearing the same
    /// clothes as a measurement, which is how the `page/` family came to be
    /// matched in a convention nothing in the document supported.
    pub(crate) fn infer(chars: &[TextChar], page_height: f32, debug: bool) -> Option<Self> {
        let usable: Vec<&TextChar> = chars
            .iter()
            .filter(|c| c.bbox.height > 0.1 && !c.char.is_whitespace())
            .collect();
        if usable.len() < 20 {
            if debug {
                eprintln!(
                    "geometry: {} usable glyphs, too few to infer from",
                    usable.len()
                );
            }
            return None;
        }

        let n = (usable.len() / 20).max(3);
        let mean_y = |slice: &[&TextChar]| -> f32 {
            slice.iter().map(|c| c.bbox.y).sum::<f32>() / slice.len() as f32
        };
        let head = mean_y(&usable[..n]);
        let tail = mean_y(&usable[usable.len() - n..]);
        let y_grows_downward = head < tail;

        // Is bbox.y the baseline rather than a box edge?
        let baseline_semantics = usable.iter().all(|c| (c.origin_y - c.bbox.y).abs() < 0.01);

        // Fallback signal when bbox.y is a real edge: under "y is the top
        // edge" boxes span [y, y+h], under "bottom edge" [y-h, y]. Count how
        // many escape the page under each reading.
        let escapes_as_top = usable
            .iter()
            .filter(|c| c.bbox.y < -1.0 || c.bbox.y + c.bbox.height > page_height + 1.0)
            .count();
        let escapes_as_bottom = usable
            .iter()
            .filter(|c| c.bbox.y - c.bbox.height < -1.0 || c.bbox.y > page_height + 1.0)
            .count();

        let space = if !y_grows_downward {
            GlyphSpace::BottomUp
        } else if baseline_semantics || escapes_as_bottom < escapes_as_top {
            // Glyphs sit above their baseline, so downward from it is wrong.
            GlyphSpace::TopDownBottom
        } else {
            GlyphSpace::TopDownTop
        };

        if debug {
            eprintln!(
                "geometry: first-glyph mean y {head:.1}, last-glyph mean y {tail:.1} => y grows {}",
                if y_grows_downward {
                    "downward"
                } else {
                    "upward"
                }
            );
            eprintln!(
                "geometry: bbox.y == origin_y for all glyphs: {baseline_semantics} \
                 (baseline semantics)"
            );
            eprintln!(
                "geometry: off-page glyphs if y is top edge: {escapes_as_top}, \
                 if bottom edge: {escapes_as_bottom} => inferred {space:?}"
            );
        }
        Some(space)
    }

    /// Glyph box as (top, bottom), measured downward from the page top.
    pub(crate) fn to_top_down(self, b: &Rect, page_height: f32) -> (f32, f32) {
        match self {
            GlyphSpace::TopDownTop => (b.y, b.y + b.height),
            GlyphSpace::TopDownBottom => (b.y - b.height, b.y),
            GlyphSpace::BottomUp => (page_height - (b.y + b.height), page_height - b.y),
        }
    }
}

/// Move glyphs out of user space and into the page frame `text_under_quads`
/// works in: same units, origin at the media box corner.
///
/// A content stream's coordinates are absolute user space and take no notice
/// of where the media box sits (ISO 32000-1, 8.3.2.3), so a page with
/// `/MediaBox [20 20 615 862]` draws its text at exactly the coordinates a
/// page at `[0 0 595 842]` does. `text_under_quads` subtracts the corner from
/// every quad, which is right — but the glyphs then have to lose it too, or
/// the two are compared in frames that differ by the origin. Twenty points on
/// PG09 to PG13 of the `page/` family: two lines of body text, and nothing
/// matched.
pub(crate) fn to_page_frame(
    mut chars: Vec<TextChar>,
    media_x0: f32,
    media_y0: f32,
) -> Vec<TextChar> {
    if media_x0 == 0.0 && media_y0 == 0.0 {
        return chars;
    }
    for c in &mut chars {
        c.bbox.x -= media_x0;
        c.bbox.y -= media_y0;
        c.origin_x -= media_x0;
        c.origin_y -= media_y0;
    }
    chars
}

/// The glyph's ink extent as (top, bottom), measured downward from the page top.
///
/// `bbox` is an em box anchored at the baseline and extending to one side of
/// it, which is fine for ordinary letters but wrong for a glyph whose ink
/// straddles its origin. A summation sign reports `ascent 0.40, descent -5.98`:
/// almost nothing above the baseline and six points below, because it is
/// centred on the mathematical axis. Taking `[origin_y, origin_y + height]`
/// puts it a full descender too high and it misses the line band entirely.
///
/// Falls back to the bbox when the font supplies no ascent or descent.
pub(crate) fn glyph_span(space: GlyphSpace, c: &TextChar, page_height: f32) -> (f32, f32) {
    if c.ascent == 0.0 && c.descent == 0.0 {
        return space.to_top_down(&c.bbox, page_height);
    }
    // descent is negative
    let above = c.origin_y + c.ascent;
    let below = c.origin_y + c.descent;
    match space {
        GlyphSpace::BottomUp => (page_height - above, page_height - below),
        // y grows downward, so ink above the baseline is at a smaller y
        GlyphSpace::TopDownTop | GlyphSpace::TopDownBottom => {
            (c.origin_y - c.ascent, c.origin_y - c.descent)
        }
    }
}

/// The baseline as a top-down coordinate.
///
/// Used to group glyphs into lines. More stable than an edge of the ink box,
/// which shifts by a couple of points between glyphs with and without
/// descenders and would split a line on that alone.
pub(crate) fn baseline_key(space: GlyphSpace, c: &TextChar, page_height: f32) -> f32 {
    match space {
        GlyphSpace::BottomUp => page_height - c.origin_y,
        GlyphSpace::TopDownTop | GlyphSpace::TopDownBottom => c.origin_y,
    }
}

/// Under --debug-geometry, the first few glyph boxes and the first quad on a
/// page: the numbers the coordinate convention has to be read from.
pub(crate) fn debug_geometry(
    page: usize,
    my0: f32,
    my1: f32,
    chars: Option<&[TextChar]>,
    annots: &[Annotation],
) {
    let Some(cs) = chars else { return };
    let Some(a) = annots.iter().find(|a| has_quads(a)) else {
        return;
    };
    eprintln!("page {}: mediabox y {my0:.1}..{my1:.1}", page + 1);
    for c in cs.iter().filter(|c| !c.char.is_whitespace()).take(3) {
        eprintln!(
            "  glyph {:?} bbox=({:.1}, {:.1}, {:.1}, {:.1}) origin_y={:.1}",
            c.char, c.bbox.x, c.bbox.y, c.bbox.width, c.bbox.height, c.origin_y
        );
    }
    if let Some(q) = a.quad_points.as_ref().and_then(|q| q.first()) {
        eprintln!("  first quad {q:?}");
    }
}
