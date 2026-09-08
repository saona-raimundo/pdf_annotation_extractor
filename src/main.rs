//! Extract highlighted text and comments from an annotated PDF.
//!
//! Text markup annotations (Highlight/Underline/StrikeOut/Squiggly) carry
//! /QuadPoints but not the text they cover, so that text is recovered by
//! intersecting each quad with the page's glyph boxes. Text and FreeText
//! annotations have no quads and yield comment-only records.
//!
//! Three pdf_oxide behaviours are worked around here:
//!   * /Contents is decoded with from_utf8_lossy, which mangles the UTF-16BE
//!     strings most annotators write. We re-decode from `raw_dict`.
//!   * The glyph coordinate convention is not reliably documented, so rather
//!     than trusting a doc comment we infer it from the glyphs themselves
//!     (see `GlyphSpace::infer`) and show the evidence under --debug-geometry.
//!   * /ActualText is re-encoded through the font in character mode, which
//!     corrupts every declared span. See `actual_text`.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use pdf_oxide::PdfDocument;
use pdf_oxide::annotation_types::AnnotationSubtype;
use pdf_oxide::annotations::Annotation;
use pdf_oxide::geometry::Rect;
use pdf_oxide::layout::TextChar;
use pdf_oxide::object::Object;
use pdf_oxide::outline::{Destination, OutlineItem};
use serde::Serialize;

mod markdown;
use markdown::{Descriptor, Numbering, Style};

mod date;

mod pdf_string;
use pdf_string::Encoding;

mod actual_text;
use actual_text::{Frame, Unit};

#[derive(Copy, Clone, PartialEq, ValueEnum)]
enum ShowArg {
    Colour,
    Kind,
    Author,
    Date,
}

impl From<ShowArg> for Descriptor {
    fn from(a: ShowArg) -> Self {
        match a {
            ShowArg::Colour => Descriptor::Colour,
            ShowArg::Kind => Descriptor::Kind,
            ShowArg::Author => Descriptor::Author,
            ShowArg::Date => Descriptor::Date,
        }
    }
}

#[derive(Copy, Clone, PartialEq, ValueEnum)]
enum NumberArg {
    None,
    Global,
    PerPage,
}

impl From<NumberArg> for Numbering {
    fn from(a: NumberArg) -> Self {
        match a {
            NumberArg::None => Numbering::None,
            NumberArg::Global => Numbering::Global,
            NumberArg::PerPage => Numbering::PerPage,
        }
    }
}

#[derive(Parser)]
#[command(about = "Extract highlights and comments from an annotated PDF")]
struct Args {
    /// Input PDF
    file: PathBuf,

    /// Output format
    #[arg(short, long, value_enum, default_value_t = Format::Markdown)]
    format: Format,

    /// Minimum fraction of a glyph's area inside a quad for it to count as
    /// covered. Raise if you pick up the neighbouring line, lower if you lose
    /// accents and descenders.
    #[arg(long, default_value_t = 0.5, hide_short_help = true)]
    min_overlap: f32,

    /// Horizontal gap that counts as a word break, as a fraction of font size.
    /// Typeset maths carries no space glyphs, so spaces are inferred from gaps.
    #[arg(long, default_value_t = 0.25, hide_short_help = true)]
    space_gap: f32,

    /// Glyph coordinate convention. `auto` infers it from the document.
    #[arg(long, value_enum, default_value_t = GeometryArg::Auto, hide_short_help = true)]
    geometry: GeometryArg,

    /// Print the measurements behind the geometry decision, then continue
    #[arg(long, hide_short_help = true)]
    debug_geometry: bool,

    /// For each quad, print every nearby glyph with its computed band position
    /// and the reason it was included or excluded. Use when covered text is
    /// missing characters and you need to see which test rejected them.
    #[arg(long, hide_short_help = true)]
    debug_quads: bool,

    /// Per-page annotation counts, including ones pdf_oxide silently drops
    #[arg(long)]
    stats: bool,

    /// Keep line-break hyphens instead of joining the split word
    #[arg(long)]
    keep_hyphens: bool,

    /// Keep ligature presentation forms (ﬁ, ﬀ) as the font maps them, instead
    /// of folding them to the letters they stand for
    #[arg(long)]
    keep_ligatures: bool,

    /// Keep each quad on its own line instead of joining into one passage
    #[arg(long)]
    split_quads: bool,

    /// Emit records with neither comment nor covered text
    #[arg(long)]
    keep_empty: bool,

    /// Extra identifiers beside the page number: colour, kind, author, date.
    /// (Markdown output only)
    #[arg(long, value_delimiter = ',', value_name = "LIST")]
    show: Vec<ShowArg>,

    /// Number the items, for citing a remark as "page 3, note 2".
    /// (Markdown output only)
    #[arg(long, value_enum, default_value_t = NumberArg::None)]
    number: NumberArg,
}

#[derive(Copy, Clone, ValueEnum)]
enum Format {
    Markdown,
    Json,
}

#[derive(Copy, Clone, PartialEq, ValueEnum)]
enum GeometryArg {
    /// Infer from glyph ordering and baseline offsets
    Auto,
    /// y measured down from page top, to the top edge of the glyph
    TopDownTop,
    /// y measured down from page top, to the bottom edge of the glyph
    TopDownBottom,
    /// y measured up from page bottom (PDF user space)
    BottomUp,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum GlyphSpace {
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
    const FALLBACK: GlyphSpace = GlyphSpace::BottomUp;

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
    fn infer(chars: &[TextChar], page_height: f32, debug: bool) -> Option<Self> {
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
    fn to_top_down(self, b: &Rect, page_height: f32) -> (f32, f32) {
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
fn to_page_frame(mut chars: Vec<TextChar>, media_x0: f32, media_y0: f32) -> Vec<TextChar> {
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
fn glyph_span(space: GlyphSpace, c: &TextChar, page_height: f32) -> (f32, f32) {
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
fn baseline_key(space: GlyphSpace, c: &TextChar, page_height: f32) -> f32 {
    match space {
        GlyphSpace::BottomUp => page_height - c.origin_y,
        GlyphSpace::TopDownTop | GlyphSpace::TopDownBottom => c.origin_y,
    }
}

#[derive(Serialize)]
struct Record {
    page: usize,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    covered_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    section: Option<String>,
    /// [x, y, width, height] in PDF user space, y-up
    rect: [f64; 4],
    link: String,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let doc = PdfDocument::open(&args.file)?;
    let file_name = args
        .file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let page_count = doc.page_count()?;
    let sections = section_titles(&doc, page_count);

    // Infer the glyph convention once, from the page in the sample that
    // carries the most text.
    //
    // This used to take the first page past 200 glyphs and, when no page
    // reached it, keep whatever the accumulator had been initialised to. A
    // document of short pages therefore matched every quad in a convention no
    // measurement supported: the `page/` fixture family runs 124 to 179
    // glyphs a page and lost all seventeen items that way, the control
    // included. Sample the richest page instead, and treat "no evidence" as a
    // state to report rather than a value to assume.
    let (space, inferred) = match args.geometry {
        GeometryArg::TopDownTop => (GlyphSpace::TopDownTop, true),
        GeometryArg::TopDownBottom => (GlyphSpace::TopDownBottom, true),
        GeometryArg::BottomUp => (GlyphSpace::BottomUp, true),
        GeometryArg::Auto => {
            let richest = (0..page_count.min(10))
                .map(|page| (doc.extract_chars(page).map(|c| c.len()).unwrap_or(0), page))
                .max_by_key(|&(len, _)| len)
                .filter(|&(len, _)| len > 0);

            let measured = richest.and_then(|(len, page)| {
                let (mx0, my0, _, my1) = doc
                    .get_page_media_box(page)
                    .unwrap_or((0.0, 0.0, 612.0, 792.0));
                let chars = to_page_frame(doc.extract_chars(page).unwrap_or_default(), mx0, my0);
                if args.debug_geometry {
                    eprintln!("geometry: inferring from page {} ({len} glyphs)", page + 1);
                }
                GlyphSpace::infer(&chars, my1 - my0, args.debug_geometry)
            });

            match measured {
                Some(space) => (space, true),
                None => (GlyphSpace::FALLBACK, false),
            }
        }
    };
    if args.debug_geometry {
        eprintln!(
            "geometry: using {space:?} ({})",
            if inferred { "measured" } else { "assumed" }
        );
    }
    // Reported at the point it can do damage rather than here: a document of
    // comment-only annotations never matches a quad and does not care.
    let mut geometry_warned = inferred;

    let mut records = Vec::new();
    let mut total_parsed = 0usize;
    let mut total_dropped = 0usize;
    let mut encoding_notes = EncodingNotes::default();

    for page in 0..page_count {
        let annots = doc.get_annotations(page)?;
        let (raw_count, inline_count) = audit_annots(&doc, page);
        total_parsed += annots.len();
        total_dropped += raw_count.saturating_sub(annots.len());

        if args.stats && raw_count > 0 {
            eprintln!(
                "page {:>3}: {} in /Annots, {} parsed{}",
                page + 1,
                raw_count,
                annots.len(),
                if inline_count > 0 {
                    format!(", {inline_count} inline dicts (dropped by pdf_oxide)")
                } else {
                    String::new()
                }
            );
        }

        if annots.is_empty() {
            continue;
        }

        let (mx0, my0, _mx1, my1) = doc.get_page_media_box(page)?;
        let page_height = my1 - my0;

        // /Rotate is not applied to anything here, and that is deliberate: it
        // is a display attribute, so a rotated page's glyphs and its
        // /QuadPoints are both still in unrotated user space and match each
        // other without a transform. All eight rotated pages of the `page/`
        // family, and the /Rotate -90 case, come out right by doing nothing.
        // Applying per-rotation offsets is where pdfannots2json's sign errors
        // live. Kept only to annotate a failure that has already happened.
        let rotation = doc.get_page_rotation(page).unwrap_or(0);

        // /ActualText, if this page declares any. Runs on the glyph list in
        // user space, before `to_page_frame`, because that is the space the
        // content stream measures in.
        let mut units: Vec<Unit> = Vec::new();
        let chars: Option<Vec<TextChar>> = if annots.iter().any(has_quads) {
            let mut cs = doc.extract_chars(page)?;
            let content = doc.get_page_content_data(page).unwrap_or_default();
            if actual_text::present(&content) {
                let fonts = page_font_widths(&doc, page);
                let (decls, unsupported) = actual_text::scan(&content, &fonts);
                let (found, broken) = actual_text::repair(&mut cs, &decls);
                units = found;
                for note in unsupported.into_iter().chain(broken) {
                    eprintln!("warning: page {}: {note}", page + 1);
                }
            }
            Some(to_page_frame(cs, mx0, my0))
        } else {
            None
        };
        let frame = Frame {
            x0: mx0,
            y0: my0,
            height: page_height,
        };

        if args.debug_geometry {
            debug_geometry(page, my0, my1, chars.as_deref(), &annots);
        }

        for a in &annots {
            if !is_interesting(&a.subtype_enum) {
                continue;
            }

            let covered_text = match (&a.quad_points, &chars) {
                (Some(quads), Some(chars)) if !quads.is_empty() => {
                    let lines = text_under_quads(
                        quads,
                        chars,
                        &Matching {
                            space,
                            frame,
                            min_overlap: args.min_overlap,
                            space_gap: args.space_gap,
                            fold_ligatures: !args.keep_ligatures,
                            debug: args.debug_quads,
                            units: &units,
                        },
                    );
                    let empty = lines.iter().filter(|l| l.text.is_empty()).count();
                    if empty > 0 {
                        if !geometry_warned {
                            eprintln!(
                                "warning: too little text to measure the glyph coordinate \
                                 convention; assumed {space:?}. Set --geometry explicitly."
                            );
                            geometry_warned = true;
                        }
                        eprintln!(
                            "warning: page {}: {empty}/{} quads matched no glyphs{} \
                             (try --min-overlap or --geometry)",
                            page + 1,
                            quads.len(),
                            if rotation % 360 != 0 {
                                format!("; page carries /Rotate {rotation}")
                            } else {
                                String::new()
                            }
                        );
                    }
                    let nonempty: Vec<Line> =
                        lines.into_iter().filter(|l| !l.text.is_empty()).collect();
                    if nonempty.is_empty() {
                        None
                    } else if args.split_quads {
                        Some(
                            nonempty
                                .iter()
                                .map(|l| l.text.clone())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    } else {
                        Some(join_lines(&nonempty, args.keep_hyphens, &units, frame))
                    }
                }
                _ => None,
            };

            // pdf_oxide's own `contents`/`author` go through from_utf8_lossy,
            // so prefer the raw dictionary and only fall back when the key is
            // genuinely absent.
            let comment = choose_text(
                raw_text(&doc, a, "Contents", &mut encoding_notes),
                a.contents.as_deref(),
            );
            let author = choose_text(
                raw_text(&doc, a, "T", &mut encoding_notes),
                a.author.as_deref(),
            );

            if covered_text.is_none() && comment.is_none() && !args.keep_empty {
                continue;
            }

            records.push(Record {
                page: page + 1,
                kind: subtype_name(&a.subtype_enum).to_string(),
                covered_text,
                comment,
                author,
                modified: a.modification_date.clone(),
                color: a.color.as_deref().and_then(hex_color),
                section: sections.get(&page).cloned(),
                rect: a.rect.unwrap_or([0.0; 4]),
                link: format!("{file_name}#page={}", page + 1),
            });
        }
    }

    if total_dropped > 0 {
        eprintln!(
            "warning: {total_dropped} of {} annotations in /Annots were not parsed \
             (rerun with --stats)",
            total_parsed + total_dropped
        );
    }

    if encoding_notes.sniffed_utf8 > 0 {
        eprintln!(
            "note: {} string(s) carried no byte order mark and were read as UTF-8 \
             rather than PDFDocEncoding",
            encoding_notes.sniffed_utf8
        );
    }
    if encoding_notes.undefined_bytes > 0 {
        eprintln!(
            "note: {} byte(s) fell on PDFDocEncoding slots the spec leaves undefined \
             (0x7F, 0x9F, 0xAD) and were read as Latin-1",
            encoding_notes.undefined_bytes
        );
    }

    sort_records(&mut records);

    let text = match args.format {
        Format::Json => format!("{}\n", serde_json::to_string_pretty(&records)?),
        Format::Markdown => {
            let style = Style {
                descriptors: args.show.iter().copied().map(Descriptor::from).collect(),
                numbering: args.number.into(),
                ..Style::default()
            };
            markdown::render(&records, &style)
        }
    };
    write_out(&text)
}

/// Write the report to the writer, treating a closed pipe as success.
///
/// `println!` panics if stdout is gone, which is what happens under
/// `tool paper.pdf | head`. It goes unnoticed on small reports because a
/// single write below the 64 KiB pipe buffer completes before the reader
/// exits; past that it aborts with exit 101 and a panic trace. Every other
/// Unix filter exits quietly instead.
fn write_to<W: Write>(mut out: W, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn write_out(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    write_to(io::stdout().lock(), text)
}

/// Count /Annots entries on a page and how many are inline dictionaries.
///
/// `get_annotations` skips any entry that is not an indirect reference, so this
/// is how annotations going missing becomes visible instead of guesswork.
fn audit_annots(doc: &PdfDocument, page: usize) -> (usize, usize) {
    let Ok(page_obj) = doc.get_page(page) else {
        return (0, 0);
    };
    let Some(dict) = page_obj.as_dict() else {
        return (0, 0);
    };
    let Some(raw) = dict.get("Annots") else {
        return (0, 0);
    };
    let resolved = doc.resolve_references(raw, 8).unwrap_or(Object::Null);
    let Some(arr) = resolved.as_array() else {
        return (0, 0);
    };
    let inline = arr
        .iter()
        .filter(|o| matches!(o, Object::Dictionary(_)))
        .count();
    (arr.len(), inline)
}

/// A text string read from the raw annotation dictionary.
///
/// The three states have to be distinguished. Collapsing `Empty` into
/// `Absent` and falling back to pdf_oxide's own field is what turned Okular's
/// empty comments into `??`: Okular writes a bare UTF-16BE BOM (`FE FF`, two
/// bytes) for an empty comment, which decodes correctly to nothing here, but
/// pdf_oxide's `from_utf8_lossy` reading of the same bytes is two replacement
/// characters. Falling back resurrected precisely the garbage this exists to
/// avoid.
enum RawString {
    /// Key not in the dictionary: use pdf_oxide's decoded field instead.
    Absent,
    /// Key present and decodes to nothing. A deliberate empty value, not a
    /// missing one, so do not fall back.
    Empty,
    Text(String),
}

/// What had to be assumed while decoding this document's strings.
///
/// Counted rather than warned about at the point of decoding: one line per
/// document is a diagnostic, one line per annotation is noise. Folds into the
/// structured `Report` when that lands.
#[derive(Default)]
struct EncodingNotes {
    /// Strings with no byte order mark, read as UTF-8 rather than as the
    /// PDFDocEncoding the spec prescribes.
    sniffed_utf8: usize,
    /// Bytes on PDFDocEncoding slots ISO 32000-1 Table D.2 leaves undefined.
    undefined_bytes: usize,
}

/// Decode a text string straight from the raw annotation dictionary.
fn raw_text(doc: &PdfDocument, a: &Annotation, key: &str, notes: &mut EncodingNotes) -> RawString {
    let Some(dict): Option<&HashMap<String, Object>> = a.raw_dict.as_ref() else {
        return RawString::Absent;
    };
    let Some(raw) = dict.get(key) else {
        return RawString::Absent;
    };
    let Ok(resolved) = doc.resolve_references(raw, 8) else {
        return RawString::Absent;
    };
    let Some(bytes) = resolved.as_string() else {
        return RawString::Absent;
    };
    let decoded = pdf_string::decode(bytes);
    if decoded.encoding == Encoding::Utf8Sniffed {
        notes.sniffed_utf8 += 1;
    }
    notes.undefined_bytes += decoded.undefined;

    let s = decoded.text.trim().to_string();
    if s.is_empty() {
        RawString::Empty
    } else {
        RawString::Text(s)
    }
}

/// Resolve a raw dictionary reading against pdf_oxide's own decoded field.
fn choose_text(raw: RawString, fallback: Option<&str>) -> Option<String> {
    match raw {
        RawString::Text(s) => Some(s),
        RawString::Empty => None,
        RawString::Absent => fallback
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    }
}

/// The text of a decoded PDF string, discarding what had to be assumed to get
/// it. See [`pdf_string`] for the encodings and the table.
///
/// Only the tests reach for this: production code wants the whole `Decoded`,
/// so that an assumption can be reported instead of silently made.
#[cfg(test)]
fn decode_pdf_string(bytes: &[u8]) -> String {
    pdf_string::decode(bytes).text
}

fn has_quads(a: &Annotation) -> bool {
    a.quad_points.as_ref().is_some_and(|q| !q.is_empty())
}

fn is_interesting(s: &AnnotationSubtype) -> bool {
    matches!(
        s,
        AnnotationSubtype::Highlight
            | AnnotationSubtype::Underline
            | AnnotationSubtype::StrikeOut
            | AnnotationSubtype::Squiggly
            | AnnotationSubtype::Text
            | AnnotationSubtype::FreeText
    )
}

fn subtype_name(s: &AnnotationSubtype) -> &'static str {
    match s {
        AnnotationSubtype::Highlight => "highlight",
        AnnotationSubtype::Underline => "underline",
        AnnotationSubtype::StrikeOut => "strikeout",
        AnnotationSubtype::Squiggly => "squiggly",
        AnnotationSubtype::Text => "note",
        AnnotationSubtype::FreeText => "freetext",
        _ => "other",
    }
}

/// What one quad, or one merged group of quads, recovered.
///
/// `right` is where the covered text ends, which the join rule needs: an empty
/// `/ActualText` declaration sitting at that edge says the glyph there stands
/// for no character, so the next line continues without a space.
struct Line {
    text: String,
    band: (f32, f32),
    right: Option<f32>,
}

/// Everything the matcher needs beyond the quads and the glyphs.
///
/// `frame` carries the media box corner and height, so it replaces the three
/// coordinates that used to be passed separately — there is now one place a
/// page's geometry comes from, and no way for a caller to hand the quads one
/// origin and the glyphs another.
struct Matching<'a> {
    space: GlyphSpace,
    frame: Frame,
    min_overlap: f32,
    space_gap: f32,
    /// Fold U+FB00–U+FB06 to the letters they stand for. See
    /// [`ligature_expansion`].
    fold_ligatures: bool,
    debug: bool,
    units: &'a [Unit],
}

/// One line per quad, in quad order. An empty `text` marks a quad that matched
/// nothing, so a geometry problem stays visible instead of being swallowed.
///
/// `chars` must already be in the page frame — see [`to_page_frame`]. Quads
/// arrive in user space and are moved into it here, as do declared units.
fn text_under_quads(quads: &[[f64; 8]], chars: &[TextChar], m: &Matching) -> Vec<Line> {
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
fn ligature_expansion(c: char) -> Option<&'static str> {
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
fn fold_glyph(c: &TextChar, letters: &str) -> Vec<TextChar> {
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

/// Does this glyph count as covered by the quad?
///
/// Vertical: the glyph's centre must sit in the quad band, with a little slack
/// for sub/superscripts. Zero-height synthetic space glyphs collapse to a point
/// and are carried by this test, where area overlap would reject them outright.
/// Horizontal: the glyph's midpoint, padded by half a glyph, so the last
/// character of a selection is not clipped by a quad that stops short.
#[allow(clippy::too_many_arguments)]
fn covers(
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
fn order_reading(
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
fn overlap_fraction(
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

fn collapse_spaces(s: &str) -> String {
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
fn join_lines(lines: &[Line], keep_hyphens: bool, units: &[Unit], frame: Frame) -> String {
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

/// `/Widths` for each simple font on a page, for the `/ActualText` scan.
///
/// Composite fonts use `/W` and a CID mapping instead; a span in one is
/// reported by `actual_text::scan` as covering no measurable text rather than
/// measured wrongly.
fn page_font_widths(doc: &PdfDocument, page: usize) -> HashMap<String, actual_text::FontWidths> {
    let mut out = HashMap::new();
    let number = |o: &Object| -> Option<f32> {
        o.as_real()
            .map(|v| v as f32)
            .or_else(|| o.as_integer().map(|v| v as f32))
    };
    let resolve = |o: &Object| doc.resolve_references(o, 8).ok();

    let Ok(page_obj) = doc.get_page(page) else {
        return out;
    };
    let fonts = page_obj
        .as_dict()
        .and_then(|d| d.get("Resources"))
        .and_then(&resolve)
        .and_then(|r| r.as_dict().and_then(|d| d.get("Font")).and_then(&resolve));
    let Some(fonts) = fonts.as_ref().and_then(|f| f.as_dict()) else {
        return out;
    };

    for (name, entry) in fonts {
        let Some(font) = resolve(entry) else { continue };
        let Some(dict) = font.as_dict() else { continue };
        let first_char = dict.get("FirstChar").and_then(&number).unwrap_or(0.0) as i64;
        let widths: Vec<f32> = dict
            .get("Widths")
            .and_then(&resolve)
            .and_then(|w| w.as_array().map(|a| a.iter().filter_map(&number).collect()))
            .unwrap_or_default();
        let missing = dict
            .get("FontDescriptor")
            .and_then(&resolve)
            .and_then(|d| {
                d.as_dict()
                    .and_then(|d| d.get("MissingWidth"))
                    .and_then(&number)
            })
            .unwrap_or(0.0);
        out.insert(
            name.clone(),
            actual_text::FontWidths {
                first_char,
                widths,
                missing,
            },
        );
    }
    out
}

fn section_titles(doc: &PdfDocument, page_count: usize) -> HashMap<usize, String> {
    let mut flat: Vec<(usize, String)> = Vec::new();
    if let Ok(Some(items)) = doc.get_outline() {
        walk(&items, &mut flat);
    }
    flat.sort_by_key(|(p, _)| *p);

    let mut map = HashMap::new();
    if flat.is_empty() {
        return map;
    }
    for page in 0..page_count {
        if let Some((_, title)) = flat.iter().rfind(|(p, _)| *p <= page) {
            map.insert(page, title.clone());
        }
    }
    map
}

fn walk(items: &[OutlineItem], out: &mut Vec<(usize, String)>) {
    for item in items {
        if let Some(Destination::PageIndex(p)) = item.dest {
            out.push((p, item.title.clone()));
        }
        walk(&item.children, out);
    }
}

fn hex_color(c: &[f64]) -> Option<String> {
    let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match c {
        [r, g, b] => Some(format!("#{:02x}{:02x}{:02x}", to8(*r), to8(*g), to8(*b))),
        [g] => {
            let v = to8(*g);
            Some(format!("#{v:02x}{v:02x}{v:02x}"))
        }
        [c_, m, y, k] => {
            let f = |x: f64| to8((1.0 - x) * (1.0 - k));
            Some(format!("#{:02x}{:02x}{:02x}", f(*c_), f(*m), f(*y)))
        }
        _ => None,
    }
}

/// `/Annots` is in creation order, not reading order. Sort down the page, then
/// left to right, using the annotation Rect (y-up, so descending y is downward).
fn sort_records(records: &mut [Record]) {
    records.sort_by(|a, b| {
        a.page.cmp(&b.page).then_with(|| {
            let ay = a.rect[1].max(a.rect[3]);
            let by = b.rect[1].max(b.rect[3]);
            by.partial_cmp(&ay)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.rect[0]
                        .partial_cmp(&b.rect[0])
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        })
    });
}

/// Under --debug-geometry, the first few glyph boxes and the first quad on a
/// page: the numbers the coordinate convention has to be read from.
fn debug_geometry(
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

/// The old signature of [`text_under_quads`], with no declarations, returning
/// just the text.
///
/// Every existing case in `tests` predates `/ActualText` handling and asserts
/// on geometry alone, so this keeps those assertions readable rather than
/// threading two arguments they do not exercise through all six of them.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn quad_texts(
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
fn join_plain(lines: &[String], keep_hyphens: bool) -> String {
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

#[cfg(test)]
mod tests;
