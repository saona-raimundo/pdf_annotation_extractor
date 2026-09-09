//! Markdown rendering.
//!
//! Everything about how the output *looks* lives here; nothing in this module
//! touches PDFs. To change the shape of a report, edit `Style` — extraction is
//! unaffected.
//!
//! Requires `crate::date::human` for `Descriptor::Date`. If `src/date.rs` does
//! not exist yet, stub it as
//! `pub(crate) fn human(_: &str) -> Option<String> { None }` and that one
//! descriptor renders nothing.

use crate::Record;

/// One block of the report.
// Variants other than the default are selected by whoever configures a Style,
// so they look unconstructed to dead-code analysis.
#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub(crate) enum Section {
    /// Everything, in reading order. The default: one flat list.
    All,
    /// Only markup and notes carrying a comment.
    Commented,
    /// Only markup with no comment attached.
    Bare,
}

/// How to render markup that carries no comment.
#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub(crate) enum BareStyle {
    /// The same block shape as a commented item, with no comment lines. The
    /// default, so a flat list keeps one shape throughout.
    Block,
    /// ` * Page #4: "commonly"` — compact, one line each.
    Quoted,
}

/// Extra visual identifiers, shown beside the page in parentheses.
///
/// A colour and a kind locate an annotation on the page the way a page number
/// locates it in the document. They are identifiers, not categories.
#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub enum Descriptor {
    /// Currently the hex value. A content-dependent namer is planned: eight
    /// yellows in one document need eight distinguishable names, which a
    /// fixed palette cannot give.
    Colour,
    /// The element itself — highlight, note, strikeout — rather than a
    /// generic grouping like "nit".
    Kind,
    Author,
    /// Formatted by `crate::date::human` from the raw PDF date syntax
    /// (`D:20260902091617+01'00'`).
    Date,
}

/// Item numbering, so a remark can be cited as "page 3, note 2".
#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub enum Numbering {
    None,
    /// Counts across the whole report.
    Global,
    /// Restarts on each page. The locator already carries the page number.
    PerPage,
}

/// How much of the Markdown syntax in a text field to neutralise.
///
/// Every level was checked against a CommonMark parser with the text placed
/// inside a list item exactly as this module emits it.
#[allow(dead_code)]
#[derive(Copy, Clone, PartialEq)]
pub(crate) enum Escaping {
    /// Pass through. A comment beginning `# ` becomes a heading and breaks the
    /// structure of the report.
    None,
    /// Backslash line-leading markers only: `#`, `>`, `-`, `+`, `*`, `=`,
    /// backtick, `~`, `_`, and `12.` / `12)`. Protects the report's structure
    /// while leaving `*emphasis*` alone, on the grounds that a comment was
    /// typed by a person who probably meant it.
    Block,
    /// Block markers plus the inline ones: backslash, backtick, `*`, `_`,
    /// `[`, `]`, `<`, `>`, `&`, `~`, `|`. For extracted text, which carries no
    /// authorial intent — a paper containing `[1](ref)` should not inject a
    /// link into the report.
    Full,
    /// Indent by four so the text becomes a code block: the bytes survive
    /// exactly, at the cost of monospace and no soft wrapping. Forces a blank
    /// line first, because an indented code block cannot interrupt a
    /// paragraph — without it the indent silently does nothing.
    Literal,
}

/// The knobs. Change these rather than the rendering functions where possible.
pub(crate) struct Style {
    /// Which blocks to emit, in order. Drop an entry to omit that block.
    pub order: Vec<Section>,
    /// Heading for a block. Empty string emits no heading at all.
    pub heading_all: &'static str,
    pub heading_commented: &'static str,
    pub heading_bare: &'static str,
    /// Prefix for each item, including leading space. Its width also fixes
    /// the item's content column, and so the quote line's indent.
    pub bullet: &'static str,
    /// Margin for the comment paragraphs. Four or more *beyond* the content
    /// column would turn them into a code block, so keep it modest.
    pub indent: &'static str,
    /// Prefix for the covered-text quote line.
    pub quote: &'static str,
    /// Written before the page number: `Page #12`.
    pub page_label: &'static str,
    /// Written after the locator: `Page #12:`.
    pub item_suffix: &'static str,
    pub bare: BareStyle,
    pub numbering: Numbering,
    /// Extra identifiers in parentheses, in this order. Empty by default.
    pub descriptors: Vec<Descriptor>,
    /// Blank line between paragraphs of a multi-paragraph comment.
    pub paragraph_gap: bool,
    pub escape_comment: Escaping,
    pub escape_covered: Escaping,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            order: vec![Section::All],
            heading_all: "## Comments",
            heading_commented: "## Detailed comments",
            heading_bare: "## Highlights",
            bullet: "- ",
            indent: "    ",
            quote: "> ",
            page_label: "Page #",
            item_suffix: ":",
            bare: BareStyle::Block,
            numbering: Numbering::None,
            descriptors: vec![],
            paragraph_gap: true,
            escape_comment: Escaping::Block,
            escape_covered: Escaping::Full,
        }
    }
}

/// Render the whole report.
///
/// Returns a String rather than printing, so the caller decides where it goes
/// and tests can assert on it.
pub(crate) fn render(records: &[Record], style: &Style) -> String {
    // Numbers are assigned over the whole report in reading order, before any
    // sectioning, so an item keeps the same number wherever it ends up.
    let numbered = assign_numbers(records, style);
    let mut out = String::new();

    for section in &style.order {
        let items: Vec<&(Option<usize>, &Record)> = numbered
            .iter()
            .filter(|(_, r)| belongs(r, *section))
            .collect();
        if items.is_empty() {
            continue;
        }

        let heading = match section {
            Section::All => style.heading_all,
            Section::Commented => style.heading_commented,
            Section::Bare => style.heading_bare,
        };
        if !heading.is_empty() {
            out.push_str(heading);
            out.push_str("\n\n");
        }

        for (n, r) in items {
            if r.comment.is_none() && style.bare == BareStyle::Quoted {
                out.push_str(&render_quoted(r, *n, style));
            } else {
                out.push_str(&render_block(r, *n, style));
            }
        }
    }

    out
}

/// Pair each record with its number, if any.
fn assign_numbers<'a>(records: &'a [Record], style: &Style) -> Vec<(Option<usize>, &'a Record)> {
    let mut out = Vec::with_capacity(records.len());
    let mut global = 0usize;
    let mut per_page: Vec<(usize, usize)> = Vec::new();

    for r in records {
        let n = match style.numbering {
            Numbering::None => None,
            Numbering::Global => {
                global += 1;
                Some(global)
            }
            Numbering::PerPage => match per_page.iter_mut().find(|(p, _)| *p == r.page) {
                Some((_, count)) => {
                    *count += 1;
                    Some(*count)
                }
                None => {
                    per_page.push((r.page, 1));
                    Some(1)
                }
            },
        };
        out.push((n, r));
    }
    out
}

/// Which block does this record belong in?
fn belongs(r: &Record, section: Section) -> bool {
    match section {
        Section::All => true,
        Section::Commented => r.comment.is_some(),
        Section::Bare => r.comment.is_none(),
    }
}

/// Compact one-line form: locator, then the covered text in quotes.
fn render_quoted(r: &Record, n: Option<usize>, style: &Style) -> String {
    let text = r
        .covered_text
        .as_deref()
        .map(|t| escape(t, style.escape_covered))
        .unwrap_or_default();
    format!(
        "{}{}{}{} \"{text}\"\n\n",
        style.bullet,
        number_prefix(n),
        locator_with(r, style),
        style.item_suffix,
    )
}

/// One shape for every annotation: locator, covered text as a quote if there
/// is any, then the comment. A single-line comment and a multi-line one render
/// identically — an earlier inline form for short comments made
/// otherwise-identical records look like different kinds of thing.
pub(crate) fn render_block(r: &Record, n: Option<usize>, style: &Style) -> String {
    let mut out = format!(
        "{}{}{}{}\n",
        style.bullet,
        number_prefix(n),
        locator_with(r, style),
        style.item_suffix,
    );

    if let Some(text) = r.covered_text.as_deref() {
        let text = escape(text, style.escape_covered);
        out.push_str(&format!("{}{}{text}\n\n", quote_indent(style), style.quote));
    }

    let paras: Vec<String> = r
        .comment
        .as_deref()
        .unwrap_or("")
        .split('\n')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| escape(p, style.escape_comment))
        .collect();

    // An indented code block cannot interrupt a paragraph, so Literal needs a
    // blank line ahead of it or the indent has no effect at all. When there is
    // covered text the quote line already supplied one.
    if style.escape_comment == Escaping::Literal && !paras.is_empty() && r.covered_text.is_none() {
        out.push('\n');
    }

    let literal_indent = if style.escape_comment == Escaping::Literal {
        "    "
    } else {
        ""
    };

    for (i, para) in paras.iter().enumerate() {
        if i > 0 && style.paragraph_gap {
            out.push('\n');
        }
        out.push_str(&format!("{}{literal_indent}{para}\n", style.indent));
    }

    out.push('\n');
    out
}

/// The indent that keeps a continuation line inside the list item: the item's
/// content column, which is the width of the bullet.
///
/// Derived rather than configured. A quote indented any less falls out of the
/// item and becomes a sibling block — the list would end at the locator and
/// the quote would render on its own — and that is not a trade anyone wants to
/// make by hand. The comment's margin stays configurable because it cannot
/// break the structure, only the look.
fn quote_indent(style: &Style) -> String {
    " ".repeat(style.bullet.chars().count())
}

fn number_prefix(n: Option<usize>) -> String {
    match n {
        Some(n) => format!("[{n}] "),
        None => String::new(),
    }
}

/// `Page #12` or `Page #12 (3.2 Method)`.
pub(crate) fn locator(r: &Record, style: &Style) -> String {
    match &r.section {
        Some(s) => format!("{}{} ({s})", style.page_label, r.page),
        None => format!("{}{}", style.page_label, r.page),
    }
}

/// The locator plus any descriptors: `Page #3 (yellow, highlight)`.
fn locator_with(r: &Record, style: &Style) -> String {
    let mut out = locator(r, style);

    let parts: Vec<String> = style
        .descriptors
        .iter()
        .filter_map(|d| match d {
            Descriptor::Colour => r.color.clone(),
            Descriptor::Kind => Some(r.kind.clone()),
            Descriptor::Author => r.author.clone(),
            Descriptor::Date => r.modified.as_deref().and_then(crate::date::human),
        })
        .collect();

    if !parts.is_empty() {
        out.push_str(&format!(" ({})", parts.join(", ")));
    }
    out
}

// ----------------------------------------------------------------- escaping

/// Neutralise Markdown syntax in one paragraph of text.
///
/// `Literal` is handled by the caller, which adds the indent and the preceding
/// blank line; here it passes the text through untouched.
fn escape(text: &str, level: Escaping) -> String {
    match level {
        Escaping::None | Escaping::Literal => text.to_string(),
        Escaping::Block => escape_leading(text, LEAD_ALL),
        // Inline first, then only those leading markers the inline pass does
        // not already cover. The two sets overlap on `*`, `_`, `>`, backtick
        // and `~`, and escaping such a character twice leaves a visible stray
        // backslash whichever order the passes run in.
        Escaping::Full => escape_leading(&escape_inline(text), LEAD_ONLY_BLOCK),
    }
}

/// Line-leading markers for `Block`: everything that can start a block.
const LEAD_ALL: &[char] = &['#', '>', '-', '+', '*', '=', '`', '~', '_'];

/// Line-leading markers for `Full`, after the inline pass has already dealt
/// with `*`, `_`, `>`, backtick and `~`.
const LEAD_ONLY_BLOCK: &[char] = &['#', '-', '+', '='];

/// Backslash a line-leading block marker — the only kind of syntax that can
/// restructure the report rather than merely restyle a phrase.
fn escape_leading(text: &str, markers: &[char]) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        let lead = &line[..line.len() - trimmed.len()];

        // `12.` and `12)` start an ordered list, and the backslash has to go
        // after the digits rather than before them.
        let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let rest = &trimmed[digits.len()..];
            if rest.starts_with('.') || rest.starts_with(')') {
                out.push_str(lead);
                out.push_str(&digits);
                out.push('\\');
                out.push_str(rest);
                continue;
            }
        }

        if trimmed.starts_with(|c: char| markers.contains(&c)) {
            out.push_str(lead);
            out.push('\\');
            out.push_str(trimmed);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Backslash the inline markers. Deliberately not every ASCII punctuation
/// character: escaping commas and full stops is permitted by the spec but
/// makes the source unreadable for no benefit.
fn escape_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '&' | '~' | '|'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Record;

    /// Deliberately duplicated from `crate::tests`: a test module that builds
    /// its own fixtures cannot be broken by a change in another one.
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

    // ------------------------------------------------------ default shape
    // Pins the default output. A deliberate change to Style::default() fails
    // here rather than drifting silently. The quote sits at the bullet's
    // width — the item's content column — and the comment at `indent`.

    #[test]
    fn renders_one_flat_list_by_default() {
        let records = vec![
            rec(3, "highlight", Some("text"), Some("Comment")),
            rec(4, "note", None, Some("note")),
        ];
        assert_eq!(
            render(&records, &Style::default()),
            "## Comments\n\n- Page #3:\n  > text\n\n    Comment\n\n- Page #4:\n    note\n\n"
        );
    }

    #[test]
    fn the_quote_sits_at_the_bullet_width() {
        // Derived, not configured: a quote indented any less falls out of the
        // list item and renders as a sibling block.
        let r = rec(1, "highlight", Some("t"), Some("c"));
        assert!(render_block(&r, None, &Style::default()).contains("\n  > t"));
        assert!(
            render_block(
                &r,
                None,
                &Style {
                    bullet: " * ",
                    ..Style::default()
                }
            )
            .contains("\n   > t")
        );
        assert!(
            render_block(
                &r,
                None,
                &Style {
                    bullet: "",
                    ..Style::default()
                }
            )
            .contains("\n> t")
        );
    }

    #[test]
    fn bare_items_render_as_blocks_by_default() {
        // One shape throughout the list: an item with no comment is just an
        // item with no comment lines.
        assert_eq!(
            render(
                &[rec(2, "highlight", Some("universe"), None)],
                &Style::default()
            ),
            "## Comments\n\n- Page #2:\n  > universe\n\n\n"
        );
    }

    #[test]
    fn quoted_bare_style_is_available() {
        assert_eq!(
            render(
                &[rec(2, "highlight", Some("universe"), None)],
                &Style {
                    bare: BareStyle::Quoted,
                    ..Style::default()
                }
            ),
            "## Comments\n\n- Page #2: \"universe\"\n\n"
        );
    }

    #[test]
    fn an_empty_heading_emits_none() {
        assert_eq!(
            render(
                &[rec(1, "note", None, Some("x"))],
                &Style {
                    heading_all: "",
                    ..Style::default()
                }
            ),
            "- Page #1:\n    x\n\n"
        );
    }

    #[test]
    fn sections_remain_available_and_ordered() {
        let records = vec![
            rec(1, "highlight", Some("b"), None),
            rec(2, "note", None, Some("c")),
        ];
        assert_eq!(
            render(
                &records,
                &Style {
                    order: vec![Section::Commented, Section::Bare],
                    ..Style::default()
                }
            ),
            "## Detailed comments\n\n- Page #2:\n    c\n\n## Highlights\n\n- Page #1:\n  > b\n\n\n"
        );
    }

    #[test]
    fn empty_sections_are_omitted() {
        let out = render(
            &[rec(1, "highlight", Some("x"), Some("y"))],
            &Style {
                order: vec![Section::Commented, Section::Bare],
                ..Style::default()
            },
        );
        assert!(out.starts_with("## Detailed comments"));
        assert!(!out.contains("## Highlights"));
    }

    // --------------------------------------------------------- descriptors
    // Colour and kind identify an annotation visually, the way a page number
    // identifies it in the document.

    #[test]
    fn descriptors_appear_in_the_order_given() {
        let mut r = rec(3, "highlight", Some("t"), Some("c"));
        r.color = Some("#ffff00".to_string());
        r.author = Some("Renée".to_string());
        r.modified = Some("D:20260902091617+01'00'".to_string());
        assert_eq!(
            render(
                &[r],
                &Style {
                    descriptors: vec![
                        Descriptor::Colour,
                        Descriptor::Kind,
                        Descriptor::Author,
                        Descriptor::Date,
                    ],
                    ..Style::default()
                }
            ),
            "## Comments\n\n- Page #3 (#ffff00, highlight, Renée, 2026-09-02 09:16):\n  > t\n\n    c\n\n"
        );
    }

    #[test]
    fn descriptors_omit_what_the_record_lacks() {
        // No colour and no date recorded: the parentheses hold only the kind,
        // rather than gaining empty slots or the word "none".
        let out = render(
            &[rec(3, "note", None, Some("c"))],
            &Style {
                descriptors: vec![Descriptor::Colour, Descriptor::Kind, Descriptor::Date],
                ..Style::default()
            },
        );
        assert!(out.contains("- Page #3 (note):"));
    }

    #[test]
    fn no_descriptors_means_no_parentheses() {
        let mut r = rec(3, "highlight", Some("t"), Some("c"));
        r.color = Some("#ffff00".to_string());
        assert!(render(&[r], &Style::default()).contains("- Page #3:"));
    }

    #[test]
    fn reproduces_the_target_layout() {
        // The layout this module was designed against, kept as a worked
        // example: if Style can no longer express it, something has regressed.
        let mut r = rec(3, "highlight", Some("text"), Some("Comment"));
        r.color = Some("#ffff00".to_string());
        assert_eq!(
            render(
                &[r],
                &Style {
                    indent: "",
                    page_label: "Page ",
                    item_suffix: "",
                    descriptors: vec![Descriptor::Colour, Descriptor::Kind],
                    ..Style::default()
                }
            ),
            "## Comments\n\n- Page 3 (#ffff00, highlight)\n  > text\n\nComment\n\n"
        );
    }

    // ----------------------------------------------------------- numbering

    #[test]
    fn numbers_globally_in_reading_order() {
        let records = vec![
            rec(1, "highlight", Some("a"), Some("c")),
            rec(2, "highlight", Some("b"), Some("c")),
        ];
        let out = render(
            &records,
            &Style {
                numbering: Numbering::Global,
                ..Style::default()
            },
        );
        assert!(out.contains("- [1] Page #1:"));
        assert!(out.contains("- [2] Page #2:"));
    }

    #[test]
    fn per_page_numbering_restarts_on_each_page() {
        let records = vec![
            rec(1, "highlight", Some("a"), Some("c")),
            rec(1, "note", None, Some("c")),
            rec(2, "highlight", Some("b"), Some("c")),
        ];
        let out = render(
            &records,
            &Style {
                numbering: Numbering::PerPage,
                ..Style::default()
            },
        );
        assert!(out.contains("- [1] Page #1:"));
        assert!(out.contains("- [2] Page #1:"));
        assert!(out.contains("- [1] Page #2:"));
    }

    #[test]
    fn numbers_are_assigned_before_sectioning() {
        // The bare item is second in reading order but rendered in the first
        // block. Numbering per section would call it [1].
        let records = vec![
            rec(1, "highlight", Some("a"), Some("c")),
            rec(2, "highlight", Some("bare"), None),
        ];
        let out = render(
            &records,
            &Style {
                order: vec![Section::Bare, Section::Commented],
                numbering: Numbering::Global,
                ..Style::default()
            },
        );
        assert!(out.find("- [2] Page #2:").unwrap() < out.find("- [1] Page #1:").unwrap());
    }

    // ------------------------------------------------------------ escaping
    // Every expectation here was checked by rendering the result through a
    // CommonMark parser: a comment beginning "# " otherwise becomes a heading
    // and breaks the structure of the report.

    #[test]
    fn block_escaping_neutralises_line_leading_markers() {
        let r = rec(1, "note", None, Some("# h\n- i\n1. i\n> q\n*em* kept"));
        assert_eq!(
            render_block(&r, None, &Style::default()),
            "- Page #1:\n    \\# h\n\n    \\- i\n\n    1\\. i\n\n    \\> q\n\n    \\*em* kept\n\n"
        );
    }

    #[test]
    fn full_escaping_covers_inline_markers_too() {
        // Extracted text carries no authorial intent, so a paper containing
        // "[1](ref)" must not inject a link into the report.
        let r = rec(1, "highlight", Some("*star* [1](r) & <b>"), None);
        assert_eq!(
            render_block(&r, None, &Style::default()),
            "- Page #1:\n  > \\*star\\* \\[1\\](r) \\& \\<b\\>\n\n\n"
        );
    }

    #[test]
    fn full_escaping_does_not_double_escape() {
        // Pins a bug in both orderings of the two passes: the leading and
        // inline marker sets overlap on `*`, `_`, `>`, backtick and `~`, and
        // escaping one of those twice leaves a visible stray backslash.
        let out = render_block(
            &rec(1, "highlight", Some("*star*"), None),
            None,
            &Style::default(),
        );
        assert!(out.contains("> \\*star\\*"), "got {out:?}");
        assert!(!out.contains("\\\\"), "double escape in {out:?}");
    }

    #[test]
    fn escaping_none_passes_text_through() {
        assert_eq!(
            render_block(
                &rec(1, "note", None, Some("# heading")),
                None,
                &Style {
                    escape_comment: Escaping::None,
                    ..Style::default()
                }
            ),
            "- Page #1:\n    # heading\n\n"
        );
    }

    #[test]
    fn literal_escaping_forces_a_blank_line_first() {
        // An indented code block cannot interrupt a paragraph. Without the
        // blank line the indent is a lazy continuation and does nothing at
        // all, which is the silent-failure case.
        assert_eq!(
            render_block(
                &rec(1, "note", None, Some("# not a heading")),
                None,
                &Style {
                    escape_comment: Escaping::Literal,
                    ..Style::default()
                }
            ),
            "- Page #1:\n\n        # not a heading\n\n"
        );
    }

    // ------------------------------------------------------- render_block
    // Pins: single-line and multi-line comments rendering in different
    // shapes. An earlier inline form for short comments made
    // otherwise-identical records look like different kinds of thing.

    #[test]
    fn formats_a_single_line_comment() {
        let r = rec(3, "highlight", Some("universe"), Some("strange choice"));
        assert_eq!(
            render_block(&r, None, &Style::default()),
            "- Page #3:\n  > universe\n\n    strange choice\n\n"
        );
    }

    #[test]
    fn formats_a_multi_line_comment_the_same_way() {
        let r = rec(
            16,
            "highlight",
            Some("Z_s,"),
            Some("Not clear what this is.\nAlso, you changed from t to s?"),
        );
        assert_eq!(
            render_block(&r, None, &Style::default()),
            "- Page #16:\n  > Z\\_s,\n\n    Not clear what this is.\n\n    Also, you changed from t to s?\n\n"
        );
    }

    #[test]
    fn formats_a_comment_with_no_covered_text() {
        let r = rec(5, "note", None, Some("Define portfolios first"));
        assert_eq!(
            render_block(&r, None, &Style::default()),
            "- Page #5:\n    Define portfolios first\n\n"
        );
    }

    #[test]
    fn drops_blank_paragraphs_from_a_comment() {
        let r = rec(19, "highlight", Some("forecasts"), Some("one?\n\n\ntwo?"));
        assert_eq!(
            render_block(&r, None, &Style::default()),
            "- Page #19:\n  > forecasts\n\n    one?\n\n    two?\n\n"
        );
    }

    #[test]
    fn paragraph_gap_can_be_turned_off() {
        assert_eq!(
            render_block(
                &rec(1, "note", None, Some("one\ntwo")),
                None,
                &Style {
                    paragraph_gap: false,
                    ..Style::default()
                }
            ),
            "- Page #1:\n    one\n    two\n\n"
        );
    }

    // ------------------------------------------------------------ locator

    #[test]
    fn locator_includes_the_section_when_known() {
        let mut r = rec(19, "highlight", None, Some("x"));
        r.section = Some("4.2 Protocol".to_string());
        assert_eq!(locator(&r, &Style::default()), "Page #19 (4.2 Protocol)");
    }

    #[test]
    fn locator_falls_back_to_the_page_alone() {
        let r = rec(19, "highlight", None, Some("x"));
        assert_eq!(locator(&r, &Style::default()), "Page #19");
    }
}
