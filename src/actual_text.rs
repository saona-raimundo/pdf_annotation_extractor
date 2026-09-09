//! `/ActualText`: what a run of glyphs is declared to mean.
//!
//! ISO 32000-1 §14.9.4. A marked-content sequence can carry `/ActualText`,
//! which says what the whole run of glyphs inside it stands for. It is not a
//! per-glyph mapping and there is no correspondence between its characters and
//! the glyphs — that is the entire point of the key. A hyphen with an empty
//! declaration stands for no character at all; a `§` declared as `Section`
//! stands for seven.
//!
//! # Why this module exists
//!
//! pdf_oxide reads the key correctly and honours the scope rules, but its
//! character-mode extractor — the one that gives us per-glyph positions, and
//! so the only one we can match quads against — materialises the replacement
//! by feeding the *already decoded* string back through the font as character
//! codes:
//!
//! ```text
//! self.show_text(actual_text.as_bytes())?;
//! ```
//!
//! Three consequences, which look like three unrelated bugs:
//!
//!   * `™` is `E2 84 A2` in UTF-8 and each byte is looked up as a glyph, so it
//!     arrives as three characters of mojibake. What they are depends on the
//!     font's own `/ToUnicode`, which is why the same defect reads differently
//!     in every document. A declared space becomes whatever the font maps code
//!     32 to — U+2423 OPEN BOX in a Latin Modern subset.
//!   * The replacement is typeset with its own metrics from the span's origin,
//!     so it occupies the width of *its* glyphs rather than the span's. Seven
//!     characters over a narrow `§` overrun into the following word, and the
//!     coordinate sort interleaves them: `Section on rounding` arrives as
//!     `Seocnti oronunding`.
//!   * A span crossing a line break is typeset entirely on the first line.
//!
//! Span mode gets the text right, but gives the declared characters an advance
//! of zero and a bounding box that stops at the real prefix, so it cannot say
//! where the span is. Neither mode alone is enough, so we take the geometry
//! from character mode and the meaning from the content stream.
//!
//! # What we do
//!
//! [`scan`] walks the content stream for the declarations and, for each, the
//! extent of the glyphs it covers — one [`Segment`] per line, because a span
//! crossing a break covers two stretches of page. [`repair`] then removes the
//! replacement pdf_oxide typeset: from the span's own origin, the characters
//! form a cursor chain, each sitting exactly one advance after the last, and
//! the real glyphs it overran are never on that chain. Removing exactly as
//! many characters as the declaration has UTF-8 bytes leaves the real text
//! untouched and hands back a [`Unit`] to be placed whole.
//!
//! Nothing here runs on a page whose content stream does not contain
//! `/ActualText`, which is very nearly all of them.
//!
//! When the upstream defect is fixed, [`repair`] is what goes: the scan and
//! the units stay, because the atomicity rule and the empty declaration are
//! ours to apply either way.

use std::collections::HashMap;

use pdf_oxide::layout::TextChar;

use crate::pdf_string;

/// How to read a user-space coordinate in the page frame the quads use.
///
/// The declarations come out of the content stream in user space; the quads
/// and the glyphs have both been moved to the media box corner and flipped to
/// measure downward from the page top. Keeping the conversion in one place is
/// the difference between this and the sign errors that make a rotated page
/// mislocate by exactly one crop box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    /// Media box left edge.
    pub x0: f32,
    /// Media box bottom edge.
    pub y0: f32,
    /// Media box height.
    pub height: f32,
}

impl Frame {
    fn x(&self, user_x: f32) -> f32 {
        user_x - self.x0
    }

    fn top_down(&self, baseline: f32) -> f32 {
        self.height - (baseline - self.y0)
    }

    /// A user-space baseline as the glyph list reports it: page frame, still
    /// measuring upward. `GlyphSpace` does the flip later, once, for both.
    fn y(&self, baseline: f32) -> f32 {
        baseline - self.y0
    }
}

/// One stretch of page covered by a declared span: the glyphs on a single line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Segment {
    /// Left edge, in user space.
    pub x: f32,
    /// Baseline, in user space.
    pub baseline: f32,
    /// Sum of the advances of the real glyphs inside the span on this line.
    pub advance: f32,
}

impl Segment {
    fn right(&self) -> f32 {
        self.x + self.advance
    }
}

/// A declaration, as read from the content stream.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Declaration {
    pub text: String,
    pub segments: Vec<Segment>,
}

/// A declaration whose replacement has been removed from the glyph list, ready
/// to be placed as a unit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Unit {
    pub text: String,
    pub segments: Vec<Segment>,
}

impl Unit {
    /// Total advance of the real glyphs the declaration covers.
    fn advance(&self) -> f32 {
        self.segments.iter().map(|s| s.advance).sum()
    }

    /// Does this declaration say that a glyph at the right-hand end of a line
    /// stands for no character?
    ///
    /// The empty declaration is not judged by coverage. In `actual_text/`
    /// AT4A the suppressed hyphen sits at x=186.71 and the quad on that line
    /// ends at exactly 186.71, so its coverage is zero however the rule is
    /// written — the producer did not select the hyphen, it selected up to it.
    /// What matters is that the thing standing for nothing is *adjacent to the
    /// break*, which is what makes the two sides contiguous.
    pub fn suppresses_break(&self, band: (f32, f32), right: f32, frame: Frame) -> bool {
        if !self.text.is_empty() {
            return false;
        }
        let height = (band.1 - band.0).max(1.0);
        self.segments.iter().any(|s| {
            let base = frame.top_down(s.baseline);
            let on_this_line = base >= band.0 - height * 0.25 && base <= band.1 + height * 0.25;
            on_this_line && frame.x(s.right()) >= right - 1.0
        })
    }

    /// The fraction of the declaration's advance that lies inside a band, and
    /// the first segment it touches.
    ///
    /// Atomicity: a quad covering part of a declared span cannot be told which
    /// part of the declared text corresponds, so the span is emitted whole
    /// above the threshold and omitted entirely below it. Falling back to
    /// per-glyph `/ToUnicode` for the covered part is the tempting third
    /// option and is wrong — the producer has said those glyphs do not stand
    /// for themselves.
    /// `band` and `x` are the quad in page space.
    pub fn coverage(
        &self,
        band: (f32, f32),
        x: (f32, f32),
        frame: Frame,
    ) -> (f32, Option<Segment>) {
        let total = self.advance();
        if total <= 0.0 {
            return (0.0, None);
        }
        let height = (band.1 - band.0).max(1.0);
        let mut inside = 0.0;
        let mut first = None;
        for s in &self.segments {
            let base = frame.top_down(s.baseline);
            if base < band.0 - height * 0.25 || base > band.1 + height * 0.25 {
                continue;
            }
            let overlap = frame.x(s.right()).min(x.1) - frame.x(s.x).max(x.0);
            if overlap > 0.0 {
                inside += overlap;
                if first.is_none() {
                    first = Some(*s);
                }
            }
        }
        (inside / total, first)
    }
}

/// Glyph-like characters for a declared unit, spread across the extent of the
/// glyphs it replaces.
///
/// Spreading is safe here in a way it is not upstream, because the decision to
/// emit at all has already been taken for the unit as a whole: these are never
/// matched individually, so the span can neither be truncated at a quad edge
/// nor emitted twice. They exist so that reading order, space synthesis and
/// line grouping keep working on one kind of thing.
///
/// The template is a real glyph from the same line, so the font, size and
/// colour are those of the text the declaration is standing in for. Cloning
/// one rather than building a `TextChar` field by field also means an upstream
/// field addition cannot silently leave us with a default.
pub(crate) fn synthesise(
    template: &TextChar,
    text: &str,
    seg: Segment,
    frame: Frame,
) -> Vec<TextChar> {
    let count = text.chars().count().max(1);
    let step = seg.advance / count as f32;
    text.chars()
        .enumerate()
        .map(|(i, ch)| {
            let mut c = template.clone();
            let x = frame.x(seg.x) + step * i as f32;
            c.char = ch;
            c.origin_x = x;
            c.origin_y = frame.y(seg.baseline);
            c.bbox.x = x;
            c.bbox.y = frame.y(seg.baseline);
            c.bbox.width = step;
            c.advance_width = step;
            c.rendered_advance = step;
            c
        })
        .collect()
}

/// Remove the replacement pdf_oxide typeset, and hand back the declarations as
/// units.
///
/// The chain walk is the whole method. From the span's origin the replacement
/// characters sit at successive cursor positions, so looking for a character at
/// the cursor and then stepping by its own advance follows the replacement and
/// nothing else: a real glyph the replacement overran is at a position the
/// cursor never visits. The count comes from the declaration — one character
/// per UTF-8 byte, because that is what the defect does.
///
/// A chain that breaks early is not repaired on a guess. The declaration is
/// dropped, a diagnostic is returned, and the caller suppresses the text
/// rather than reporting characters it cannot vouch for.
pub(crate) fn repair(chars: &mut Vec<TextChar>, decls: &[Declaration]) -> (Vec<Unit>, Vec<String>) {
    const EPS: f32 = 0.05;

    let mut remove = vec![false; chars.len()];
    let mut units = Vec::new();
    let mut diagnostics = Vec::new();

    for d in decls {
        let Some(anchor) = d.segments.first() else {
            diagnostics.push(format!(
                "declaration {:?} covers no shown text; nothing to place it against",
                d.text
            ));
            continue;
        };

        let wanted = d.text.len(); // bytes, deliberately: see above
        let mut chain = Vec::with_capacity(wanted);
        let mut cursor = anchor.x;
        for _ in 0..wanted {
            let hit = chars.iter().enumerate().position(|(i, c)| {
                !remove[i]
                    && !chain.contains(&i)
                    && (c.origin_y - anchor.baseline).abs() < EPS
                    && (c.origin_x - cursor).abs() < EPS
            });
            match hit {
                Some(i) => {
                    cursor = chars[i].origin_x + chars[i].rendered_advance;
                    chain.push(i);
                }
                None => break,
            }
        }

        if chain.len() != wanted {
            diagnostics.push(format!(
                "declaration {:?}: found {} of {} replacement characters at x={:.2}; \
                 text suppressed",
                d.text,
                chain.len(),
                wanted,
                anchor.x
            ));
            continue;
        }
        for i in chain {
            remove[i] = true;
        }
        units.push(Unit {
            text: d.text.clone(),
            segments: d.segments.clone(),
        });
    }

    let mut i = 0;
    chars.retain(|_| {
        let keep = !remove[i];
        i += 1;
        keep
    });
    (units, diagnostics)
}

/// Is it worth scanning this page at all?
pub(crate) fn present(content: &[u8]) -> bool {
    content.windows(11).any(|w| w == b"/ActualText")
}

// ---------------------------------------------------------------- the scan

/// Character widths for one simple font, as `/Widths` gives them.
#[derive(Clone, Debug, Default)]
pub(crate) struct FontWidths {
    pub first_char: i64,
    pub widths: Vec<f32>,
    pub missing: f32,
}

impl FontWidths {
    fn width(&self, code: u8) -> f32 {
        let i = code as i64 - self.first_char;
        if i >= 0 && (i as usize) < self.widths.len() {
            self.widths[i as usize]
        } else {
            self.missing
        }
    }
}

/// One token of a content stream. Only what the text operators need.
#[derive(Clone, Debug, PartialEq)]
enum Token {
    Num(f32),
    Str(Vec<u8>),
    Name(String),
    Op(String),
    /// `<<`, `>>`, `[`, `]` — kept so an operand list can be read positionally
    /// without building the objects.
    Punct,
}

/// Walk the content stream for declarations and the extent of what they cover.
///
/// Segments are opened lazily, at the first text shown after a positioning
/// operator, not when the scope opens. Producers write `ET /Span<<…>>BDC BT …
/// Td`, so at `BDC` the text position is still wherever the *previous* line
/// ended — anchoring there puts the span two lines from where it belongs.
///
/// A declaration that opens mid-string, with no positioning operator between
/// the last shown text and the `BDC`, would need the advance of every glyph
/// before it to place the anchor. Nothing in the corpus does this: it is
/// reported rather than guessed at.
pub(crate) fn scan(
    content: &[u8],
    fonts: &HashMap<String, FontWidths>,
) -> (Vec<Declaration>, Vec<String>) {
    let mut decls = Vec::new();
    let mut unsupported = Vec::new();
    // One entry per open marked-content scope, `None` for a scope that
    // declares nothing, so that EMC pops the right one.
    let mut open: Vec<Option<Declaration>> = Vec::new();
    let mut operands: Vec<Token> = Vec::new();

    let mut cursor = Cursor::default();
    let mut state = TextState::default();
    let mut font: Option<String> = None;

    for token in tokenise(content) {
        let Token::Op(op) = &token else {
            operands.push(token);
            continue;
        };
        let op = op.clone();
        let nums: Vec<f32> = operands
            .iter()
            .filter_map(|t| match t {
                Token::Num(n) => Some(*n),
                _ => None,
            })
            .collect();

        match op.as_str() {
            "BT" => cursor.set(0.0, 0.0),
            "Tm" if nums.len() >= 6 => cursor.set(nums[4], nums[5]),
            "Td" if nums.len() >= 2 => {
                cursor.set(cursor.line.0 + nums[0], cursor.line.1 + nums[1]);
            }
            "TD" if nums.len() >= 2 => {
                state.leading = -nums[1];
                cursor.set(cursor.line.0 + nums[0], cursor.line.1 + nums[1]);
            }
            "T*" => cursor.next_line(state.leading),
            "TL" if !nums.is_empty() => state.leading = nums[0],
            "Tc" if !nums.is_empty() => state.char_spacing = nums[0],
            "Tw" if !nums.is_empty() => state.word_spacing = nums[0],
            "Tz" if !nums.is_empty() => state.horizontal = nums[0] / 100.0,
            "Tf" => {
                for t in &operands {
                    if let Token::Name(n) = t {
                        font = Some(n.clone());
                    }
                }
                if let Some(n) = nums.last() {
                    state.size = *n;
                }
            }
            "Tj" | "'" | "\"" => {
                if op != "Tj" {
                    cursor.next_line(state.leading);
                }
                let mut last: Option<Vec<u8>> = None;
                for t in &operands {
                    if let Token::Str(s) = t {
                        last = Some(s.clone());
                    }
                }
                if let Some(s) = last {
                    show(&s, &mut cursor, &mut open, fonts, font.as_deref(), &state);
                }
            }
            "TJ" => {
                for t in operands.clone() {
                    match t {
                        Token::Str(s) => {
                            show(&s, &mut cursor, &mut open, fonts, font.as_deref(), &state);
                        }
                        Token::Num(n) => {
                            cursor.adjust(-n / 1000.0 * state.size * state.horizontal, &mut open);
                        }
                        Token::Name(_) | Token::Op(_) | Token::Punct => {}
                    }
                }
            }
            "BDC" | "BMC" => match declared_text(&operands) {
                Some(text) => {
                    if cursor.pos.is_none() {
                        unsupported.push(format!(
                            "declaration {text:?} opens outside BT/ET; text suppressed"
                        ));
                        open.push(None);
                    } else {
                        open.push(Some(Declaration {
                            text,
                            segments: Vec::new(),
                        }));
                    }
                }
                None => open.push(None),
            },
            "EMC" => {
                if let Some(Some(d)) = open.pop() {
                    if d.segments.is_empty() {
                        unsupported.push(format!(
                            "declaration {:?} covers no shown text; text suppressed",
                            d.text
                        ));
                    } else {
                        decls.push(d);
                    }
                }
            }
            _ => {}
        }
        operands.clear();
    }

    if !open.is_empty() {
        unsupported.push(format!("{} marked-content scope(s) left open", open.len()));
    }
    (decls, unsupported)
}

/// Where the next glyph goes.
///
/// `pos` is the text matrix translation and `line` the line matrix's: `Td` is
/// relative to the start of the line, never to the cursor, so the two have to
/// be kept apart. `fresh` records whether the position has been set since text
/// was last shown, which is what makes a segment start where the glyphs start.
#[derive(Default)]
struct Cursor {
    pos: Option<(f32, f32)>,
    line: (f32, f32),
    fresh: bool,
}

impl Cursor {
    fn set(&mut self, x: f32, y: f32) {
        self.pos = Some((x, y));
        self.line = (x, y);
        self.fresh = true;
    }

    fn next_line(&mut self, leading: f32) {
        self.set(self.line.0, self.line.1 - leading);
    }

    /// A TJ kern. It moves the cursor, so it belongs to the extent of whatever
    /// span is open — but only once that span has glyphs, or a kern before the
    /// first one would stretch the segment leftwards from nothing.
    fn adjust(&mut self, dx: f32, open: &mut [Option<Declaration>]) {
        if let Some(p) = self.pos.as_mut() {
            p.0 += dx;
        }
        if self.fresh {
            return;
        }
        for d in open.iter_mut().flatten() {
            if let Some(s) = d.segments.last_mut() {
                s.advance += dx;
            }
        }
    }
}

/// The text state operators that affect an advance.
struct TextState {
    size: f32,
    char_spacing: f32,
    word_spacing: f32,
    horizontal: f32,
    leading: f32,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal: 1.0,
            leading: 0.0,
        }
    }
}

/// Advance the cursor by a shown string, extending every open declaration.
fn show(
    bytes: &[u8],
    cursor: &mut Cursor,
    open: &mut [Option<Declaration>],
    fonts: &HashMap<String, FontWidths>,
    font: Option<&str>,
    state: &TextState,
) {
    let Some(pos) = cursor.pos else { return };
    let metrics = font.and_then(|f| fonts.get(f));
    let mut total = 0.0;
    for &code in bytes {
        let w0 = metrics.map_or(0.0, |m| m.width(code)) / 1000.0;
        let spacing = if code == b' ' {
            state.word_spacing
        } else {
            0.0
        };
        total += (w0 * state.size + state.char_spacing + spacing) * state.horizontal;
    }
    for d in open.iter_mut().flatten() {
        if cursor.fresh || d.segments.is_empty() {
            d.segments.push(Segment {
                x: pos.0,
                baseline: pos.1,
                advance: 0.0,
            });
        }
        if let Some(s) = d.segments.last_mut() {
            s.advance += total;
        }
    }
    cursor.pos = Some((pos.0 + total, pos.1));
    cursor.fresh = false;
}

/// The `/ActualText` value out of a `BDC` operand list, if there is one.
///
/// Both string forms occur and a reader that handles only one misses cases:
/// every value in the `actual_text/` fixture is a hex string carrying a
/// UTF-16BE mark, `<FEFF…>`, except the empty one, which is a literal `()`.
/// The mark has to come off, or every declared string is prefixed with U+FEFF
/// and fails for a reason that has nothing to do with `/ActualText`.
fn declared_text(operands: &[Token]) -> Option<String> {
    let at = operands
        .iter()
        .position(|t| matches!(t, Token::Name(n) if n == "ActualText"))?;
    operands[at + 1..].iter().find_map(|t| match t {
        Token::Str(s) => Some(pdf_string::decode(s).text),
        _ => None,
    })
}

/// Tokenise a content stream: numbers, strings, names, operators.
///
/// Not a parser. Dictionaries and arrays are flattened into their contents,
/// which is enough because every operator we care about reads its operands
/// positionally and `/ActualText` is found by name.
fn tokenise(data: &[u8]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' | b'\0' | 0x0C => i += 1,
            b'%' => {
                while i < data.len() && data[i] != b'\n' {
                    i += 1;
                }
            }
            b'(' => {
                let (s, next) = literal_string(data, i + 1);
                out.push(Token::Str(s));
                i = next;
            }
            b'<' if data.get(i + 1) == Some(&b'<') => {
                out.push(Token::Punct);
                i += 2;
            }
            b'>' if data.get(i + 1) == Some(&b'>') => {
                out.push(Token::Punct);
                i += 2;
            }
            b'<' => {
                let (s, next) = hex_string(data, i + 1);
                out.push(Token::Str(s));
                i = next;
            }
            b'[' | b']' | b'{' | b'}' | b')' | b'>' => {
                out.push(Token::Punct);
                i += 1;
            }
            b'/' => {
                let start = i + 1;
                i = start;
                while i < data.len() && !is_delimiter(data[i]) {
                    i += 1;
                }
                out.push(Token::Name(
                    String::from_utf8_lossy(&data[start..i]).into_owned(),
                ));
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < data.len() && matches!(data[i], b'0'..=b'9' | b'.' | b'-' | b'+') {
                    i += 1;
                }
                match std::str::from_utf8(&data[start..i])
                    .ok()
                    .and_then(|s| s.parse::<f32>().ok())
                {
                    Some(n) => out.push(Token::Num(n)),
                    // Malformed numbers exist; dropping the token is enough,
                    // because every operator reads its operands positionally
                    // and a missing one simply fails its arity guard.
                    None => out.push(Token::Punct),
                }
            }
            _ => {
                let start = i;
                while i < data.len() && !is_delimiter(data[i]) {
                    i += 1;
                }
                if i == start {
                    i += 1;
                    continue;
                }
                out.push(Token::Op(
                    String::from_utf8_lossy(&data[start..i]).into_owned(),
                ));
            }
        }
    }
    out
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b' ' | b'\t'
            | b'\r'
            | b'\n'
            | b'\0'
            | 0x0C
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

/// A literal string, from just past the opening parenthesis.
fn literal_string(data: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut depth = 1usize;
    while i < data.len() {
        match data[i] {
            b'\\' => {
                let Some(&next) = data.get(i + 1) else { break };
                i += 2;
                match next {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'\n' => {}
                    b'\r' => {
                        if data.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    b'0'..=b'7' => {
                        let mut v = (next - b'0') as u32;
                        let mut taken = 1;
                        while taken < 3 {
                            match data.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    v = v * 8 + (d - b'0') as u32;
                                    i += 1;
                                    taken += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push((v & 0xFF) as u8);
                    }
                    other => out.push(other),
                }
            }
            b'(' => {
                depth += 1;
                out.push(b'(');
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    break;
                }
                out.push(b')');
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    (out, i)
}

/// A hex string, from just past the opening angle bracket.
fn hex_string(data: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    let mut digits = Vec::new();
    while i < data.len() && data[i] != b'>' {
        if data[i].is_ascii_hexdigit() {
            digits.push(data[i]);
        }
        i += 1;
    }
    if digits.len() % 2 == 1 {
        digits.push(b'0'); // ISO 32000-1 §7.3.4.3: an odd final digit gets a 0
    }
    let bytes = digits
        .chunks(2)
        .filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect();
    (bytes, i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths() -> HashMap<String, FontWidths> {
        // One em per glyph, so at size 10 every character advances exactly 10
        // and the arithmetic in the tests stays readable.
        let mut m = HashMap::new();
        m.insert(
            "F1".to_string(),
            FontWidths {
                first_char: 32,
                widths: vec![1000.0; 96],
                missing: 1000.0,
            },
        );
        m
    }

    #[test]
    fn reads_a_hex_declaration_and_its_extent() {
        // <FEFF0053...> is "Section"; the span covers one glyph of advance 10.
        let content = b"BT /F1 10 Tf 100 700 Td [(the )]TJ \
                        /Span<</ActualText<FEFF00530065006300740069006F006E>>>BDC \
                        [(\xA7)]TJ EMC [( on)]TJ ET";
        let (decls, unsupported) = scan(content, &widths());
        assert!(unsupported.is_empty(), "{unsupported:?}");
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].text, "Section");
        assert_eq!(
            decls[0].segments,
            vec![Segment {
                x: 140.0,
                baseline: 700.0,
                advance: 10.0
            }]
        );
    }

    #[test]
    fn reads_an_empty_literal_declaration() {
        // Pins the form a reader handling only <...> misses: the empty value
        // arrives as a literal (), and it is the one that says "this glyph
        // stands for no character".
        let content = b"BT /F1 10 Tf 100 700 Td [(infra)]TJ \
                        /Span<</ActualText()>>BDC [(-)]TJ EMC ET";
        let (decls, _) = scan(content, &widths());
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].text, "");
        assert_eq!(decls[0].segments[0].x, 150.0);
    }

    #[test]
    fn strips_the_byte_order_mark() {
        // Keeping it prefixes every declared string with U+FEFF and fails the
        // family for a reason unrelated to /ActualText.
        let content = b"BT /F1 10 Tf 0 0 Td /Span<</ActualText<FEFF0041>>>BDC [(x)]TJ EMC ET";
        let (decls, _) = scan(content, &widths());
        assert_eq!(decls[0].text, "A");
    }

    #[test]
    fn anchors_at_the_text_not_at_the_bdc() {
        // The producer form: ET, then BDC, then BT and an absolute Td. At BDC
        // the cursor is still on the previous line, so anchoring there puts
        // the span in the wrong place entirely.
        let content = b"BT /F1 10 Tf 100 700 Td [(the)]TJ ET \
                        /Span<</ActualText<FEFF0041>>>BDC \
                        BT /F1 10 Tf 200 500 Td [(x)]TJ ET EMC";
        let (decls, _) = scan(content, &widths());
        assert_eq!(decls[0].segments[0].x, 200.0);
        assert_eq!(decls[0].segments[0].baseline, 500.0);
    }

    #[test]
    fn a_span_across_a_line_break_is_two_segments() {
        let content = b"BT /F1 10 Tf 100 700 Td \
                        /Span<</ActualText<FEFF0041>>>BDC \
                        [(third)]TJ -20 -14 Td [(of)]TJ EMC ET";
        let (decls, _) = scan(content, &widths());
        assert_eq!(decls[0].segments.len(), 2);
        assert_eq!(decls[0].segments[0].advance, 50.0);
        assert_eq!(decls[0].segments[1].baseline, 686.0);
        assert_eq!(decls[0].segments[1].advance, 20.0);
    }

    #[test]
    fn tj_kerning_counts_toward_the_advance() {
        // A kern inside the span moves the cursor and so belongs to the
        // extent; ignoring it makes the span narrower than its ink. The sign
        // is the one that catches people: a TJ number is *subtracted* from the
        // displacement, so -500 moves right by half an em, which is how
        // pdftex writes an inter-word space.
        let content = b"BT /F1 10 Tf 0 0 Td /Span<</ActualText<FEFF0041>>>BDC \
                        [(ab)-500(cd)]TJ EMC ET";
        let (decls, _) = scan(content, &widths());
        assert_eq!(decls[0].segments[0].advance, 20.0 + 5.0 + 20.0);
    }

    #[test]
    fn a_scope_without_a_declaration_is_not_one() {
        let content = b"BT /F1 10 Tf 0 0 Td /P <</MCID 0>> BDC [(ab)]TJ EMC ET";
        let (decls, unsupported) = scan(content, &widths());
        assert!(decls.is_empty());
        assert!(unsupported.is_empty(), "{unsupported:?}");
    }

    #[test]
    fn nested_scopes_pop_in_order() {
        let content = b"BT /F1 10 Tf 0 0 Td /P<</MCID 0>>BDC \
                        /Span<</ActualText<FEFF0041>>>BDC [(ab)]TJ EMC \
                        [(cd)]TJ EMC ET";
        let (decls, unsupported) = scan(content, &widths());
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0].text, "A");
        assert_eq!(decls[0].segments[0].advance, 20.0);
        assert!(unsupported.is_empty(), "{unsupported:?}");
    }

    #[test]
    fn a_declaration_covering_nothing_is_reported() {
        let content = b"BT /F1 10 Tf 0 0 Td /Span<</ActualText<FEFF0041>>>BDC EMC ET";
        let (decls, unsupported) = scan(content, &widths());
        assert!(decls.is_empty());
        assert_eq!(unsupported.len(), 1);
    }

    #[test]
    fn present_finds_the_key() {
        assert!(present(b"q /Span<</ActualText<FEFF>>>BDC EMC Q"));
        assert!(!present(b"BT /F1 10 Tf (plain) Tj ET"));
    }

    #[test]
    fn hex_strings_pad_an_odd_digit() {
        let (bytes, _) = hex_string(b"41 4>", 0);
        assert_eq!(bytes, vec![0x41, 0x40]);
    }

    #[test]
    fn literal_strings_handle_escapes_and_nesting() {
        let (bytes, _) = literal_string(b"a\\(b\\) (c) \\101\\n)", 0);
        assert_eq!(bytes, b"a(b) (c) A\n");
    }
}
