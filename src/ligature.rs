//! Folding presentation-form ligatures to the letters they stand for.
//!
//! Strictly U+FB00–U+FB06, the Alphabetic Presentation Forms that are purely
//! typographic. Æ, œ and ß are letters of their languages and are never
//! touched. Folding happens after quad matching, so a ligature is still atomic
//! for coverage, and after `/ActualText`, so a declaration is never undone.

use pdf_oxide::layout::TextChar;

/// The letters a ligature presentation form stands for, or `None` if the
/// character is not one.
///
/// The fold set is the whole of the difficulty, and it is exactly U+FB00 to
/// U+FB06. That block, Alphabetic Presentation Forms, exists for legacy
/// round-tripping and holds no letter of any language. A note reading `tariﬀ`
/// cannot be searched for or pasted anywhere useful, which is why folding is
/// the default rather than the option.
///
/// What must **not** be folded, and why each is tempting:
///
/// * **U+00C6/E6 (Æ/æ)** is a letter of Danish, Norwegian and Icelandic.
///   `cli-pdf-extract` folds it to `fl` and corrupts every Danish word that
///   contains it.
/// * **U+0152/0153 (Œ/œ)** is a letter of French, and Unicode *names* it
///   `LATIN SMALL LIGATURE OE`. The name records its history, not its status.
/// * **U+00DF (ß)** is a letter of German.
///
/// Nor is NFKC a shortcut: it also rewrites superscript two as an ASCII `2`,
/// which would mangle exponents throughout the papers this tool exists for.
///
/// U+FB05 is the one judgement call. Its constituent letters are U+017F LONG S
/// and `t`, so that is what it folds to — U+017F is a letter, like Æ, and not
/// this function's business. Folding it to `st` would be more useful to
/// someone searching an early-modern text and is a second transformation, not
/// a de-ligaturing.
pub(crate) fn ligature_expansion(c: char) -> Option<&'static str> {
    match c {
        '\u{FB00}' => Some("ff"),
        '\u{FB01}' => Some("fi"),
        '\u{FB02}' => Some("fl"),
        '\u{FB03}' => Some("ffi"),
        '\u{FB04}' => Some("ffl"),
        '\u{FB05}' => Some("\u{017F}t"),
        '\u{FB06}' => Some("st"),
        _ => None,
    }
}

/// Split one glyph into the letters its presentation form stands for.
///
/// The advance is divided between them, which is what pdf_oxide already does
/// for a `/ToUnicode` entry mapping one code to several characters. It matters
/// for more than tidiness: `order_reading` decides where to insert a space
/// from `origin_x + rendered_advance`, so pieces that each carried the whole
/// advance would fabricate a gap after every ligature — `staﬀ on` becoming
/// `staff  on` — and pieces that carried none would swallow a real space.
pub(crate) fn fold_glyph(c: &TextChar, letters: &str) -> Vec<TextChar> {
    let count = letters.chars().count().max(1) as f32;
    let advance = c.rendered_advance / count;
    let ink = c.bbox.width / count;
    letters
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let mut piece = c.clone();
            piece.char = ch;
            piece.origin_x = c.origin_x + advance * i as f32;
            piece.bbox.x = c.bbox.x + ink * i as f32;
            piece.bbox.width = ink;
            piece.advance_width = c.advance_width / count;
            piece.rendered_advance = advance;
            piece
        })
        .collect()
}
