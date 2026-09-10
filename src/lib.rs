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

use pdf_oxide::PdfDocument;
use pdf_oxide::layout::TextChar;
use serde::Serialize;

pub mod markdown;
pub use markdown::{BareStyle, Descriptor, Escaping, Numbering, Section, Style};

mod date;

mod pdf_string;

mod actual_text;
use actual_text::{Frame, Unit, Unrepaired};

pub mod diagnostic;
pub use diagnostic::{Diagnostic, Diagnostics, Severity};

pub mod error;
pub use error::Error;

mod glyphs;
pub use glyphs::GlyphSpace;

mod document;
mod ligature;
mod matching;

use document::{
    EncodingNotes, audit_annots, choose_text, has_quads, hex_color, is_interesting,
    page_font_widths, raw_text, section_titles, subtype_name,
};
use glyphs::{debug_geometry, to_page_frame};
use matching::{Line, Matching, join_lines, quads_reach_segments, text_under_quads};

#[derive(Debug, Serialize)]
pub struct Record {
    pub page: usize,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub covered_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// [x, y, width, height] in PDF user space, y-up
    pub rect: [f64; 4],
    pub link: String,
    /// What had to be assumed or given up on for *this* item.
    ///
    /// Skipped when empty, so a document that extracted cleanly serialises
    /// byte for byte as it did before the field existed. Only annotation-scope
    /// diagnostics appear here; document-scope ones go to stderr, and will
    /// join a `Report` alongside the annotations when the JSON stops being a
    /// bare array.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

/// How to read a document.
///
/// `#[non_exhaustive]` with public fields rather than a builder: there are
/// eleven knobs already and the review adds more, so eleven setters would be
/// eleven more things to keep in step, while `Options::default()` followed by
/// assignment reads the same and stays non-breaking when a twelfth arrives.
/// The commitment being made is that the field *names and types* are API —
/// which is the one thing here that gets expensive after 1.0.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Fraction of a glyph that must fall inside a quad to be claimed by it.
    pub min_overlap: f32,
    /// Gap between glyphs, as a fraction of font size, that becomes a space.
    pub space_gap: f32,
    /// The glyph coordinate convention, or `None` to measure it from the
    /// document.
    pub geometry: Option<GlyphSpace>,
    /// Keep a line-break hyphen instead of joining the word across the break.
    pub keep_hyphens: bool,
    /// Leave U+FB00–U+FB06 as the font mapped them.
    pub keep_ligatures: bool,
    /// One line of covered text per quad rather than one per annotation.
    pub split_quads: bool,
    /// Emit annotations that have neither covered text nor a comment.
    pub keep_empty: bool,
    /// What to put in each record's `link`. `extract` takes bytes and so has
    /// no filename of its own; without this the fragment is `#page=N` alone.
    pub document_name: Option<String>,
    /// Collect a `PageAnnotCounts` diagnostic per page. Off by default because
    /// a 500-page document produces 500 of them.
    pub page_counts: bool,
    /// Trace geometry inference to stderr.
    ///
    /// A library writing to stderr is wrong, and these two are the exception
    /// that proves it: they are a per-glyph trace, read once while diagnosing
    /// a bad extraction. Doing it properly means `Options<'a>` holding a
    /// `&mut dyn Write`, and a lifetime on the whole API to serve a debug aid
    /// is a poor trade. Worth revisiting if anything but a terminal ever wants
    /// them.
    pub debug_geometry: bool,
    /// Trace quad-to-glyph matching to stderr. See `debug_geometry`.
    pub debug_quads: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            min_overlap: 0.5,
            space_gap: 0.25,
            geometry: None,
            keep_hyphens: false,
            keep_ligatures: false,
            split_quads: false,
            keep_empty: false,
            document_name: None,
            page_counts: false,
            debug_geometry: false,
            debug_quads: false,
        }
    }
}

/// What one document yielded.
///
/// The annotations and what had to be assumed to get them, together, because
/// the second is only useful beside the first. The JSON the CLI writes is
/// still a bare array of `annotations` — whether the wire format becomes an
/// object with both is a schema decision, and this type does not settle it.
#[derive(Debug)]
pub struct Report {
    pub annotations: Vec<Record>,
    pub diagnostics: Diagnostics,
}

/// Read every annotation worth reporting out of one PDF.
///
/// Takes the bytes rather than a path: there is nothing here that needs a
/// filesystem, and the only reason it ever did was that `PdfDocument::open`
/// was the constructor to hand. `from_bytes` is what `open` calls internally
/// after reading the file, so this is the same code path with one fewer
/// assumption about where the document came from — which is what lets the same
/// function serve a CLI, a test harness and a browser.
pub fn extract(bytes: Vec<u8>, opts: &Options) -> Result<Report, Error> {
    let mut diags = Diagnostics::default();
    let doc = PdfDocument::from_bytes(bytes).map_err(Error::parse)?;
    let file_name = opts.document_name.clone().unwrap_or_default();

    let page_count = doc.page_count().map_err(Error::page_count)?;
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
    let (space, inferred) = match opts.geometry {
        Some(space) => (space, true),
        None => {
            let richest = (0..page_count.min(10))
                .map(|page| (doc.extract_chars(page).map(|c| c.len()).unwrap_or(0), page))
                .max_by_key(|&(len, _)| len)
                .filter(|&(len, _)| len > 0);

            let measured = richest.and_then(|(len, page)| {
                let (mx0, my0, _, my1) = doc
                    .get_page_media_box(page)
                    .unwrap_or((0.0, 0.0, 612.0, 792.0));
                let chars = to_page_frame(doc.extract_chars(page).unwrap_or_default(), mx0, my0);
                if opts.debug_geometry {
                    eprintln!("geometry: inferring from page {} ({len} glyphs)", page + 1);
                }
                GlyphSpace::infer(&chars, my1 - my0, opts.debug_geometry)
            });

            match measured {
                Some(space) => (space, true),
                None => (GlyphSpace::FALLBACK, false),
            }
        }
    };
    if opts.debug_geometry {
        eprintln!(
            "geometry: using {space:?} ({})",
            if inferred { "measured" } else { "assumed" }
        );
    }
    // `inferred` is false when no page carried enough text to measure the
    // convention. The warning is raised at the first quad that fails to match
    // rather than here: a document of comment-only annotations never compares
    // a glyph to a quad and is not affected by the assumption.

    let mut records = Vec::new();
    let mut total_parsed = 0usize;
    let mut total_dropped = 0usize;
    let mut encoding_notes = EncodingNotes::default();

    for page in 0..page_count {
        // A page that will not parse costs a page's worth of annotations, not
        // the document's. Everything else in this function is deliberately
        // lenient — `unwrap_or_default`, `unwrap_or(0)` — and these three `?`s
        // were the exception: one malformed page threw away every annotation
        // in the file, including the pages that were fine. Which is the worst
        // possible outcome for a tool whose whole job is salvage.
        //
        // Reported on stderr for now, in the style of the warnings around
        // them; folds into `Diagnostic` with the rest in the next phase.
        let annots = match doc.get_annotations(page) {
            Ok(annots) => annots,
            Err(e) => {
                diags.push(Diagnostic::AnnotsUnreadable {
                    page: page + 1,
                    error: e.to_string(),
                });
                continue;
            }
        };
        let (raw_count, inline_count) = audit_annots(&doc, page);
        total_parsed += annots.len();
        total_dropped += raw_count.saturating_sub(annots.len());

        if opts.page_counts && raw_count > 0 {
            diags.push(Diagnostic::PageAnnotCounts {
                page: page + 1,
                in_annots: raw_count,
                parsed: annots.len(),
                inline: inline_count,
            });
        }

        if annots.is_empty() {
            continue;
        }

        // No media box, and none inherited from /Pages. Skipped rather than
        // defaulted to Letter: the height is what flips every quad into
        // top-down space, so a guessed one does not degrade the answer, it
        // moves every annotation on the page to a plausible wrong place.
        let Ok((mx0, my0, _mx1, my1)) = doc.get_page_media_box(page) else {
            diags.push(Diagnostic::NoMediaBox {
                page: page + 1,
                annotations: annots.len(),
            });
            continue;
        };
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
        let mut unrepaired: Vec<Unrepaired> = Vec::new();
        let chars: Option<Vec<TextChar>> = if annots.iter().any(has_quads) {
            // `None`, not an empty glyph list. An empty list would match no
            // quads and then blame the geometry — `quads matched no glyphs
            // (try --min-overlap or --geometry)` — sending the reader after a
            // convention that was never the problem.
            match doc.extract_chars(page) {
                Ok(mut cs) => {
                    let content = doc.get_page_content_data(page).unwrap_or_default();
                    if actual_text::present(&content) {
                        let fonts = page_font_widths(&doc, page);
                        let (decls, unsupported) = actual_text::scan(&content, &fonts, page + 1);
                        let (found, broken) = actual_text::repair(&mut cs, &decls, page + 1);
                        units = found;
                        for d in unsupported {
                            diags.push(d);
                        }
                        // Every broken declaration reaches the document
                        // channel whether or not an annotation covers it; the
                        // ones that are covered are attached to those items
                        // below.
                        for u in &broken {
                            diags.push(u.diagnostic.clone());
                        }
                        unrepaired = broken;
                    }
                    Some(to_page_frame(cs, mx0, my0))
                }
                Err(e) => {
                    diags.push(Diagnostic::TextUnextractable {
                        page: page + 1,
                        error: e.to_string(),
                    });
                    None
                }
            }
        } else {
            None
        };
        let frame = Frame {
            x0: mx0,
            y0: my0,
            height: page_height,
        };

        if opts.debug_geometry {
            debug_geometry(page, my0, my1, chars.as_deref(), &annots);
        }

        for a in &annots {
            if !is_interesting(&a.subtype_enum) {
                continue;
            }

            let mut item_diags: Vec<Diagnostic> = Vec::new();

            let covered_text = match (&a.quad_points, &chars) {
                (Some(quads), Some(chars)) if !quads.is_empty() => {
                    let lines = text_under_quads(
                        quads,
                        chars,
                        &Matching {
                            space,
                            frame,
                            min_overlap: opts.min_overlap,
                            space_gap: opts.space_gap,
                            fold_ligatures: !opts.keep_ligatures,
                            debug: opts.debug_quads,
                            units: &units,
                        },
                    );
                    let empty = lines.iter().filter(|l| l.text.is_empty()).count();
                    if empty > 0 {
                        if !inferred {
                            let assumed = Diagnostic::GeometryAssumed { space };
                            if !diags.contains(&assumed) {
                                diags.push(assumed);
                            }
                        }
                        // Both channels: the record so a JSON consumer reading
                        // this one item knows not to trust its covered_text,
                        // and the document so someone reading stderr or
                        // Markdown still sees it.
                        let unmatched = Diagnostic::UnmatchedQuads {
                            page: page + 1,
                            unmatched: empty,
                            total: quads.len(),
                            rotation,
                        };
                        item_diags.push(unmatched.clone());
                        diags.push(unmatched);
                    }
                    let nonempty: Vec<Line> =
                        lines.into_iter().filter(|l| !l.text.is_empty()).collect();
                    if nonempty.is_empty() {
                        None
                    } else if opts.split_quads {
                        Some(
                            nonempty
                                .iter()
                                .map(|l| l.text.clone())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    } else {
                        Some(join_lines(&nonempty, opts.keep_hyphens, &units, frame))
                    }
                }
                _ => None,
            };

            // A broken declaration leaves its mojibake in the glyph list, so
            // this item's covered_text may contain characters the document
            // said read as something else — and nothing about the geometry
            // looks wrong, because the quads matched glyphs. This is the only
            // signal, so it has to travel with the item.
            if let Some(quads) = a.quad_points.as_ref() {
                for u in &unrepaired {
                    if quads_reach_segments(quads, &u.segments) {
                        item_diags.push(u.diagnostic.clone());
                    }
                }
            }

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

            if covered_text.is_none() && comment.is_none() && !opts.keep_empty {
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
                // Filtered rather than trusted. Nothing is lost by dropping a
                // document-scope diagnostic here, because `diags` already has
                // it — what is prevented is a record claiming a fact about
                // the page as a fact about itself.
                diagnostics: item_diags
                    .into_iter()
                    .filter(Diagnostic::is_annotation_scope)
                    .collect(),
            });
        }
    }

    if total_dropped > 0 {
        diags.push(Diagnostic::UnparsedAnnots {
            dropped: total_dropped,
            total: total_parsed + total_dropped,
        });
    }

    if encoding_notes.sniffed_utf8 > 0 {
        diags.push(Diagnostic::EncodingSniffed {
            count: encoding_notes.sniffed_utf8,
        });
    }
    if encoding_notes.undefined_bytes > 0 {
        diags.push(Diagnostic::UndefinedPdfDocBytes {
            count: encoding_notes.undefined_bytes,
        });
    }

    sort_records(&mut records);

    Ok(Report {
        annotations: records,
        diagnostics: diags,
    })
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

#[cfg(test)]
mod tests;
