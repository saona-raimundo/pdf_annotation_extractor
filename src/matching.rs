//! Which glyphs a quad claims, and how they become a line of text.
//!
//! The failure mode of everything here is a plausible wrong answer: text from
//! the line above, a word clipped at a quad edge, a space that was never in
//! the document. Each rule below is the residue of one of those.

use pdf_oxide::layout::TextChar;

use crate::actual_text;
use crate::actual_text::Frame;
use crate::actual_text::Segment;
use crate::actual_text::Unit;
use crate::glyphs::GlyphSpace;
use crate::glyphs::baseline_key;
use crate::glyphs::glyph_span;
use crate::ligature::fold_glyph;
use crate::ligature::ligature_expansion;

/// Does this annotation's selection reach the extent of a declaration?
///
/// Both sides are in raw user space, y-up, before the media box origin is
/// subtracted — quads as `/QuadPoints` gave them, segments as the content
/// stream measured them — so no frame is needed and no top-down flip happens.
///
/// A segment is a horizontal run at a baseline, so the test is: the x ranges
/// meet, and the baseline falls inside the quad vertically. The baseline of a
/// span lies inside any quad that selects it, because that is what selecting
/// text means; a quad tight enough to exclude it would match no glyphs on that
/// line either.
///
/// Deliberately generous where it is uncertain. This decides whether a warning
/// is attached, and a warning on a neighbouring item costs a moment's
/// attention, while a missing one costs the reader a wrong quotation they had
/// no reason to doubt.
pub(crate) fn quads_reach_segments(quads: &[[f64; 8]], segments: &[Segment]) -> bool {
    quads.iter().any(|quad| {
        let xs = [quad[0], quad[2], quad[4], quad[6]];
        let ys = [quad[1], quad[3], quad[5], quad[7]];
        let qx0 = xs.iter().cloned().fold(f64::INFINITY, f64::min) as f32;
        let qx1 = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) as f32;
        let qy0 = ys.iter().cloned().fold(f64::INFINITY, f64::min) as f32;
        let qy1 = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max) as f32;
        segments.iter().any(|s| {
            s.baseline >= qy0 && s.baseline <= qy1 && s.x <= qx1 && (s.x + s.advance) >= qx0
        })
    })
}

/// What one quad, or one merged group of quads, recovered.
///
/// `right` is where the covered text ends, which the join rule needs: an empty
/// `/ActualText` declaration sitting at that edge says the glyph there stands
/// for no character, so the next line continues without a space.
pub(crate) struct Line {
    pub(crate) text: String,
    pub(crate) band: (f32, f32),
    pub(crate) right: Option<f32>,
}

/// Everything the matcher needs beyond the quads and the glyphs.
///
/// `frame` carries the media box corner and height, so it replaces the three
/// coordinates that used to be passed separately — there is now one place a
/// page's geometry comes from, and no way for a caller to hand the quads one
/// origin and the glyphs another.
pub(crate) struct Matching<'a> {
    pub(crate) space: GlyphSpace,
    pub(crate) frame: Frame,
    pub(crate) min_overlap: f32,
    pub(crate) space_gap: f32,
    /// Fold U+FB00–U+FB06 to the letters they stand for. See
    /// [`ligature_expansion`].
    pub(crate) fold_ligatures: bool,
    pub(crate) debug: bool,
    pub(crate) units: &'a [Unit],
}

/// One line per quad, in quad order. An empty `text` marks a quad that matched
/// nothing, so a geometry problem stays visible instead of being swallowed.
///
/// `chars` must already be in the page frame — see [`to_page_frame`]. Quads
/// arrive in user space and are moved into it here, as do declared units.
pub(crate) fn text_under_quads(quads: &[[f64; 8]], chars: &[TextChar], m: &Matching) -> Vec<Line> {
    let (media_x0, media_y0, page_height) = (m.frame.x0, m.frame.y0, m.frame.height);
    let (space, min_overlap, space_gap, debug) = (m.space, m.min_overlap, m.space_gap, m.debug);
    let units = m.units;
    let frame = m.frame;
    // Quad bands in top-down page space. Producers do not agree on the order
    // of the /QuadPoints array — some store it bottom-up — so sort the bands
    // into reading order ourselves rather than trusting the array.
    let mut bands: Vec<(f32, f32, f32, f32)> = quads
        .iter()
        .map(|quad| {
            // Corner order within a quad also varies, so take the extent over
            // all four points rather than trusting their positions.
            let xs = [quad[0], quad[2], quad[4], quad[6]];
            let ys = [quad[1], quad[3], quad[5], quad[7]];
            let qx0 = xs.iter().cloned().fold(f64::INFINITY, f64::min) as f32 - media_x0;
            let qx1 = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) as f32 - media_x0;
            let uy0 = ys.iter().cloned().fold(f64::INFINITY, f64::min) as f32 - media_y0;
            let uy1 = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max) as f32 - media_y0;
            (qx0, qx1, page_height - uy1, page_height - uy0)
        })
        .collect();
    bands.sort_by(|a, b| {
        a.2.partial_cmp(&b.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
    });

    // (band top, band bottom, recovered characters, declared characters).
    //
    // Glyphs are matched unfolded and folded on the way in here, which is the
    // only order that keeps both rules: a glyph is atomic for coverage — a
    // ligature is one glyph and a quad either takes it or does not — while the
    // text that comes out is the letters it stands for.
    let mut groups: Vec<(f32, f32, Vec<TextChar>, Vec<TextChar>)> = Vec::new();
    // A declaration is placed once, however many quads touch it: it declares
    // what the whole run means, so a second emission is a second reading of
    // the same words.
    let mut placed = vec![false; units.len()];
    // A glyph belongs to exactly one quad. Without this, a character close to
    // the boundary of two overlapping bands — a stacked limit sitting within
    // the main line's slack, say — is emitted twice.
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();

    for (bi, (qx0, qx1, qtop, qbottom)) in bands.iter().copied().enumerate() {
        if debug {
            eprintln!(
                "quad {bi}: x {qx0:.2}..{qx1:.2}  band(top-down) {qtop:.2}..{qbottom:.2}  \
                 height {:.2}",
                qbottom - qtop
            );
        }
        let mut picked: Vec<usize> = Vec::new();
        for (i, c) in chars.iter().enumerate() {
            if claimed.contains(&i) {
                continue;
            }
            let (gtop, gbottom) = glyph_span(space, c, page_height);
            let hit = covers(c, gtop, gbottom, qx0, qx1, qtop, qbottom, min_overlap);

            // Trace anything horizontally near this quad, hit or miss: a
            // character missing from the output was rejected by one of these
            // tests, and this shows which.
            if debug {
                let mid_x = c.bbox.x + c.bbox.width / 2.0;
                if mid_x >= qx0 - 12.0 && mid_x <= qx1 + 12.0 {
                    let band = (qbottom - qtop).max(1.0);
                    let mid_y = (gtop + gbottom) / 2.0;
                    let inside = (gbottom.min(qbottom) - gtop.max(qtop)).max(0.0);
                    eprintln!(
                        "  {} {:?} mid_x {mid_x:.2} span {gtop:.2}..{gbottom:.2} \
                         mid_y {mid_y:.2} h {:.2} band_cover {:.2} \
                         bbox.y {:.2} origin_y {:.2} asc {:.2} desc {:.2}",
                        if hit { "KEEP" } else { "drop" },
                        c.char,
                        gbottom - gtop,
                        inside / band,
                        c.bbox.y,
                        c.origin_y,
                        c.ascent,
                        c.descent,
                    );
                }
            }

            if hit {
                picked.push(i);
            }
        }
        claimed.extend(picked.iter().copied());

        let mut recovered: Vec<TextChar> = Vec::new();
        for &i in &picked {
            let glyph = &chars[i];
            match ligature_expansion(glyph.char).filter(|_| m.fold_ligatures) {
                Some(letters) => recovered.extend(fold_glyph(glyph, letters)),
                None => recovered.push(glyph.clone()),
            }
        }

        // Declared spans, judged as wholes. A quad covering part of one cannot
        // be told which part of the declared text corresponds, so it is
        // emitted entire above the threshold and omitted entirely below it.
        let mut declared: Vec<TextChar> = Vec::new();
        for (ui, unit) in units.iter().enumerate() {
            if placed[ui] || unit.text.is_empty() {
                continue;
            }
            let (fraction, segment) = unit.coverage((qtop, qbottom), (qx0, qx1), frame);
            if fraction < 0.5 {
                continue;
            }
            let Some(segment) = segment else { continue };
            // Font, size and colour come from a real glyph on the same line
            // where there is one: the declared text stands in for that text.
            let template = picked.first().map(|&i| &chars[i]).or_else(|| chars.first());
            if let Some(template) = template {
                declared.extend(actual_text::synthesise(
                    template, &unit.text, segment, frame,
                ));
                placed[ui] = true;
            }
        }
        if debug && !declared.is_empty() {
            eprintln!(
                "  declared span placed: {:?}",
                declared.iter().map(|c| c.char).collect::<String>()
            );
        }
        // Merge this band into the previous line group when the two overlap
        // vertically. Some producers emit one quad per glyph rather than one
        // per line — Papers does on rotated pages — and treating each as its
        // own line inserts a space between every character. Assembling the
        // union instead lets order_reading space them from the actual gaps.
        let merged = match groups.last_mut() {
            Some((gtop, gbottom, hits, extra)) => {
                let overlap = (qbottom.min(*gbottom) - qtop.max(*gtop)).max(0.0);
                let shorter = (qbottom - qtop).min(*gbottom - *gtop).max(1.0);
                if overlap / shorter > 0.5 {
                    *gtop = gtop.min(qtop);
                    *gbottom = gbottom.max(qbottom);
                    hits.append(&mut recovered);
                    extra.append(&mut declared);
                    true
                } else {
                    false
                }
            }
            None => false,
        };
        if !merged {
            groups.push((qtop, qbottom, recovered, declared));
        }
    }

    groups
        .into_iter()
        .map(|(top, bottom, recovered, declared)| {
            let mut hits: Vec<&TextChar> = recovered.iter().collect();
            hits.extend(declared.iter());
            let right = hits
                .iter()
                .map(|c| c.origin_x + c.rendered_advance)
                .fold(f32::NEG_INFINITY, f32::max);
            Line {
                text: collapse_spaces(order_reading(&hits, space, page_height, space_gap).trim()),
                band: (top, bottom),
                right: (right > f32::NEG_INFINITY).then_some(right),
            }
        })
        .collect()
}

/// Does this glyph count as covered by the quad?
///
/// Vertical: the glyph's centre must sit in the quad band, with a little slack
/// for sub/superscripts. Zero-height synthetic space glyphs collapse to a point
/// and are carried by this test, where area overlap would reject them outright.
/// Horizontal: the glyph's midpoint, padded by half a glyph, so the last
/// character of a selection is not clipped by a quad that stops short.
#[allow(clippy::too_many_arguments)]
pub(crate) fn covers(
    c: &TextChar,
    gtop: f32,
    gbottom: f32,
    qx0: f32,
    qx1: f32,
    qtop: f32,
    qbottom: f32,
    min_overlap: f32,
) -> bool {
    let band = (qbottom - qtop).max(1.0);
    let mid_y = (gtop + gbottom) / 2.0;
    // Quad bands usually span the full line box, so keep this tight — every
    // point of slack reaches toward the neighbouring line.
    let slack = band * 0.25;
    let vertical = mid_y >= qtop - slack && mid_y <= qbottom + slack;

    let mid_x = c.bbox.x + c.bbox.width / 2.0;
    // Numerical slack only. A half-glyph pad was needed while the vertical
    // geometry was wrong and quads were landing off by a line; with the
    // correct convention the quads bound the selection accurately, and a pad
    // of that size is the same order as the overshoot of a trailing period —
    // so it admitted the period after one sentinel and not another.
    let pad = 0.25_f32;
    let horizontal = mid_x >= qx0 - pad && mid_x <= qx1 + pad;

    if vertical && horizontal {
        return true;
    }

    // Large operators (summation, integral, big delimiters) are far taller
    // than the line band, so their centre falls outside it. Judge them by how
    // much of the band they span at that x position instead. Gated on being
    // substantially taller than the band, or ordinary glyphs from the
    // neighbouring line would qualify too.
    let glyph_height = gbottom - gtop;
    if horizontal && glyph_height > band * 1.3 {
        let inside = (gbottom.min(qbottom) - gtop.max(qtop)).max(0.0);
        if inside / band >= 0.6 {
            return true;
        }
    }

    // Fallback for tall glyphs (large math) whose centre falls outside a band
    // that nonetheless covers most of them. Set --min-overlap 0.95 to disable.
    overlap_fraction(
        c.bbox.x,
        c.bbox.x + c.bbox.width,
        gtop,
        gbottom,
        qx0,
        qx1,
        qtop,
        qbottom,
    ) >= min_overlap
}

/// Assemble glyphs in reading order: group into lines by the glyph's lower
/// edge, order lines down the page, order glyphs left to right within a line.
///
/// Spaces are synthesised from horizontal gaps. Typeset maths does not emit
/// space glyphs — `$\mu$` after a word is positioned, not spaced — so relying
/// on the character stream alone yields `location𝜇`. `space_gap` is the gap, as
/// a fraction of font size, that counts as a word break; pdfminer calls the
/// same knob `--word-margin`.
pub(crate) fn order_reading(
    hits: &[&TextChar],
    space: GlyphSpace,
    page_height: f32,
    space_gap: f32,
) -> String {
    if hits.is_empty() {
        return String::new();
    }

    let mut keyed: Vec<(f32, &TextChar)> = hits
        .iter()
        .map(|c| (baseline_key(space, c, page_height), *c))
        .collect();
    keyed.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.1.bbox
                    .x
                    .partial_cmp(&b.1.bbox.x)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    // Tolerance from typical glyph height, so subscripts stay on their line.
    let mut heights: Vec<f32> = hits
        .iter()
        .map(|c| c.bbox.height)
        .filter(|h| *h > 0.1)
        .collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let tol = heights
        .get(heights.len() / 2)
        .copied()
        .unwrap_or(10.0)
        .max(1.0)
        * 0.6;

    let mut lines: Vec<Vec<&TextChar>> = Vec::new();
    let mut current_key = keyed[0].0;
    let mut current: Vec<&TextChar> = Vec::new();
    for (key, c) in keyed {
        if (key - current_key).abs() > tol && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_key = key;
        }
        current.push(c);
    }
    if !current.is_empty() {
        lines.push(current);
    }

    let mut out = String::new();
    for (i, mut line) in lines.into_iter().enumerate() {
        line.sort_by(|a, b| {
            a.bbox
                .x
                .partial_cmp(&b.bbox.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if i > 0 && !out.ends_with(' ') {
            out.push(' ');
        }

        let mut prev_end: Option<f32> = None;
        for c in line {
            if let Some(end) = prev_end {
                let gap = c.origin_x - end;
                let threshold = c.font_size.max(1.0) * space_gap;
                if gap > threshold && !out.ends_with(' ') && !c.char.is_whitespace() {
                    out.push(' ');
                }
            }
            out.push(c.char);
            // rendered_advance, not bbox.width: for a ligature the bbox width
            // is the ink width divided across the expanded characters, so it
            // underestimates the cursor and fabricates a gap after "fi"/"ff".
            prev_end = Some(c.origin_x + c.rendered_advance);
        }
    }
    out
}

/// Fraction of the glyph box lying inside the quad.
#[allow(clippy::too_many_arguments)]
pub(crate) fn overlap_fraction(
    gx0: f32,
    gx1: f32,
    gtop: f32,
    gbottom: f32,
    qx0: f32,
    qx1: f32,
    qtop: f32,
    qbottom: f32,
) -> f32 {
    let ix = gx1.min(qx1) - gx0.max(qx0);
    let iy = gbottom.min(qbottom) - gtop.max(qtop);
    if ix <= 0.0 || iy <= 0.0 {
        return 0.0;
    }
    let area = (gx1 - gx0) * (gbottom - gtop);
    if area <= 0.0 {
        return 1.0; // zero-area glyphs are all-or-nothing
    }
    (ix * iy) / area
}

pub(crate) fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        let is_space = ch.is_whitespace();
        if is_space && prev_space {
            continue;
        }
        out.push(if is_space { ' ' } else { ch });
        prev_space = is_space;
    }
    out
}

/// Join the per-quad lines into one passage.
///
/// Three cases at a break, in order of how much the document tells us:
///
/// 1. An empty `/ActualText` at the end of the earlier line. The document has
///    said that glyph stands for no character, so the two sides are
///    contiguous: no space and no hyphen, and `--keep-hyphens` does not change
///    it, because there is nothing left to guess. This is `actual_text/` AT4A.
/// 2. A trailing hyphen and a lowercase continuation. Ambiguous — a
///    discretionary break and a compound word look identical in an untagged
///    PDF — so `--keep-hyphens` exists and neither setting is right for every
///    case. This is `text/` TX4, and the contrast with AT4A is the point.
/// 3. Anything else: one space.
pub(crate) fn join_lines(
    lines: &[Line],
    keep_hyphens: bool,
    units: &[Unit],
    frame: Frame,
) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == 0 {
            out.push_str(&line.text);
            continue;
        }
        let previous = &lines[i - 1];
        let suppressed = previous.right.is_some_and(|right| {
            units
                .iter()
                .any(|u| u.suppresses_break(previous.band, right, frame))
        });
        if suppressed {
            out.push_str(&line.text);
            continue;
        }
        let dehyphenate = !keep_hyphens
            && (out.ends_with('-') || out.ends_with('\u{2010}'))
            && line.text.chars().next().is_some_and(|c| c.is_lowercase());
        if dehyphenate {
            out.pop();
            out.push_str(&line.text);
        } else {
            if !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(&line.text);
        }
    }
    out
}

/// The old signature of [`text_under_quads`], with no declarations, returning
/// just the text.
///
/// Every existing case in `tests` predates `/ActualText` handling and asserts
/// on geometry alone, so this keeps those assertions readable rather than
/// threading two arguments they do not exercise through all six of them.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn quad_texts(
    quads: &[[f64; 8]],
    chars: &[TextChar],
    space: GlyphSpace,
    media_x0: f32,
    media_y0: f32,
    page_height: f32,
    min_overlap: f32,
    space_gap: f32,
    debug: bool,
) -> Vec<String> {
    let frame = Frame {
        x0: media_x0,
        y0: media_y0,
        height: page_height,
    };
    text_under_quads(
        quads,
        chars,
        &Matching {
            space,
            frame,
            min_overlap,
            space_gap,
            fold_ligatures: true,
            debug,
            units: &[],
        },
    )
    .into_iter()
    .map(|l| l.text)
    .collect()
}

/// [`join_lines`] over plain strings, with no declarations in play.
#[cfg(test)]
pub(crate) fn join_plain(lines: &[String], keep_hyphens: bool) -> String {
    let lines: Vec<Line> = lines
        .iter()
        .map(|text| Line {
            text: text.clone(),
            band: (0.0, 10.0),
            right: None,
        })
        .collect();
    join_lines(
        &lines,
        keep_hyphens,
        &[],
        Frame {
            x0: 0.0,
            y0: 0.0,
            height: 792.0,
        },
    )
}
