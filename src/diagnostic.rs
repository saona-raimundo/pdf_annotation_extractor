//! What had to be assumed, worked around, or given up on.
//!
//! The failure mode of this tool is not a crash, it is a plausible wrong
//! answer: a quad that matched the line above, a comment read in the wrong
//! encoding, a page skipped. So the warnings are not incidental output, they
//! are half the product, and they were previously scattered across a dozen
//! `eprintln!` calls plus a struct of counters — three channels with three
//! shapes, none of which a test could see.
//!
//! This module is the one channel. Extraction *collects* diagnostics; the
//! caller decides where they go. That split is what lets the fixture harness
//! assert on them once it calls a library rather than a binary, and it is what
//! lets the browser build show them next to the annotation they belong to.
//!
//! # Scope
//!
//! A diagnostic is either about the document or about one annotation, and the
//! distinction is not cosmetic. "Half the quads on page 3 matched nothing" is
//! useless to someone reading item 47 of a JSON array unless it is attached to
//! item 47. So per-annotation diagnostics ride on the record, where a consumer
//! deciding whether to trust one item can see them, *and* on the document
//! collector, so that someone reading stderr or Markdown still gets the
//! warning. Two channels for one fact, because which one the user is reading
//! is not ours to guess.
//!
//! Each variant below says which it is. Of the `/ActualText` ones only
//! `DeclarationUnrepaired` carries a position, so it is the only one that can
//! be attributed to the annotations whose quads cover it; the rest describe
//! the page or the content stream and stay at document scope.
//!
//! # What this is not
//!
//! `--debug-geometry` and `--debug-quads` stay on `eprintln!`. They are a
//! trace, not a diagnostic: one line per glyph considered, emitted only when
//! asked for, read once and thrown away. Collecting them would mean holding a
//! page of text in memory to print it unchanged.

use std::fmt;

use serde::Serialize;

use crate::GlyphSpace;

/// How much attention a diagnostic deserves.
///
/// Ordered, so `--strict` can be expressed as a threshold rather than a match.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Information that was asked for. Printed without a prefix, because it is
    /// an answer rather than a complaint: `--stats` reports counts, and
    /// labelling those `note:` would misdescribe them.
    Info,
    /// Something was assumed, and the assumption is probably right. The output
    /// is usable; the reader should know what it rests on.
    Note,
    /// Something is missing, or may be wrong. `--strict` fails on these.
    Warning,
}

impl Severity {
    fn prefix(self) -> &'static str {
        match self {
            Severity::Info => "",
            Severity::Note => "note: ",
            Severity::Warning => "warning: ",
        }
    }
}

/// One thing worth telling the reader about this document.
///
/// Page numbers are stored **1-based**, as printed. Storing the 0-based index
/// and adding one in `Display` puts the same `+ 1` in a dozen places and makes
/// the omission of one of them invisible.
///
/// Serialised internally tagged, so a per-annotation diagnostic in the JSON
/// reads as `{"kind": "unmatched_quads", "page": 3, ...}`. There is
/// deliberately no rendered `message` field: the prose belongs to stderr, and
/// duplicating it into the machine channel would make it something consumers
/// parse. If it turns out to be wanted, it belongs in the wire layer with the
/// rest of the schema decisions.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Diagnostic {
    /// No page carried enough text to measure the glyph coordinate
    /// convention, so a convention was assumed.
    ///
    /// Raised at the first quad that fails to match rather than at inference
    /// time: a document of comment-only annotations never compares a glyph to
    /// a quad and is not affected by the assumption.
    GeometryAssumed { space: GlyphSpace },

    /// `--stats`: how many entries a page's `/Annots` holds and how many
    /// became annotations.
    PageAnnotCounts {
        page: usize,
        in_annots: usize,
        parsed: usize,
        /// Inline dictionaries, which `get_annotations` skips.
        inline: usize,
    },

    /// Total `/Annots` entries that never became annotations.
    UnparsedAnnots { dropped: usize, total: usize },

    /// A page's `/Annots` could not be read at all.
    AnnotsUnreadable { page: usize, error: String },

    /// No `/MediaBox` on the page and none inherited from `/Pages`.
    NoMediaBox { page: usize, annotations: usize },

    /// A page's text could not be extracted, so its markup has no covered
    /// text.
    TextUnextractable { page: usize, error: String },

    /// Quads on a page that claimed no glyphs.
    ///
    /// **Annotation scope.** Computed per annotation and always was — the
    /// counts are that annotation's quads, not the page's — but it used to be
    /// printed as `page N: ...`, so a page with three bad annotations produced
    /// three lines that looked like three facts about the page. It now also
    /// travels on the record it belongs to.
    UnmatchedQuads {
        page: usize,
        unmatched: usize,
        total: usize,
        /// `/Rotate`, mentioned only when non-zero. Rotation is not applied
        /// anywhere — glyphs and quads are both in unrotated user space — but
        /// it is the first thing to suspect when a rotated page is the one
        /// that failed, so it is worth naming rather than leaving the reader
        /// to find it.
        rotation: i32,
    },

    /// Strings with no byte order mark that were read as UTF-8 rather than as
    /// the PDFDocEncoding the spec prescribes.
    EncodingSniffed { count: usize },

    /// Bytes landing on PDFDocEncoding slots Table D.2 leaves undefined.
    UndefinedPdfDocBytes { count: usize },

    /// An `/ActualText` scope opened outside `BT`/`ET`, so there is no text
    /// position to anchor it to.
    DeclarationOutsideText { page: usize, text: String },

    /// An `/ActualText` scope closed without any text having been shown
    /// inside it. The declaration describes nothing.
    DeclarationCoversNoText { page: usize, text: String },

    /// A declaration reached `repair` with no segments.
    ///
    /// Unreachable from `scan`, which drops empty-segment declarations before
    /// returning them, so this only fires for a caller that builds
    /// declarations itself. Kept because `repair` is public within the crate
    /// and the alternative is an index panic.
    DeclarationUnanchored { page: usize, text: String },

    /// The chain of re-encoded replacement characters was not found whole, so
    /// the declared text could not be repaired and was **suppressed**.
    ///
    /// **Annotation scope**, for every annotation whose selection overlaps the
    /// declaration's extent, and document scope besides.
    ///
    /// The most important thing in this module. Every other variant says the
    /// output may be wrong; this one says the output contains characters the
    /// document explicitly said read as something else — and the geometry
    /// looks fine, because the quads did match glyphs. Nothing else reveals
    /// it.
    DeclarationUnrepaired {
        page: usize,
        text: String,
        found: usize,
        wanted: usize,
        x: f32,
    },

    /// Marked-content scopes still open at the end of the content stream.
    ScopesLeftOpen { page: usize, count: usize },
}

impl Diagnostic {
    pub(crate) fn severity(&self) -> Severity {
        match self {
            Diagnostic::PageAnnotCounts { .. } => Severity::Info,
            Diagnostic::EncodingSniffed { .. } | Diagnostic::UndefinedPdfDocBytes { .. } => {
                Severity::Note
            }
            Diagnostic::GeometryAssumed { .. }
            | Diagnostic::UnparsedAnnots { .. }
            | Diagnostic::AnnotsUnreadable { .. }
            | Diagnostic::NoMediaBox { .. }
            | Diagnostic::TextUnextractable { .. }
            | Diagnostic::UnmatchedQuads { .. }
            | Diagnostic::DeclarationOutsideText { .. }
            | Diagnostic::DeclarationCoversNoText { .. }
            | Diagnostic::DeclarationUnanchored { .. }
            | Diagnostic::DeclarationUnrepaired { .. }
            | Diagnostic::ScopesLeftOpen { .. } => Severity::Warning,
        }
    }

    /// Does this belong on the record as well as on the document?
    ///
    /// Used so the extractor cannot attach a document-scope diagnostic to an
    /// annotation by accident, and so adding a variant forces the question to
    /// be answered rather than defaulted.
    pub(crate) fn is_annotation_scope(&self) -> bool {
        matches!(
            self,
            Diagnostic::UnmatchedQuads { .. } | Diagnostic::DeclarationUnrepaired { .. }
        )
    }
}

/// The message, without its severity prefix.
///
/// Every string here reproduces what the `eprintln!` it replaced emitted,
/// byte for byte, so that unifying the channel is provably not a change to
/// what the user reads. The tests below pin that.
impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Diagnostic::GeometryAssumed { space } => write!(
                f,
                "too little text to measure the glyph coordinate convention; \
                 assumed {space:?}. Set --geometry explicitly."
            ),
            Diagnostic::PageAnnotCounts {
                page,
                in_annots,
                parsed,
                inline,
            } => {
                write!(f, "page {page:>3}: {in_annots} in /Annots, {parsed} parsed")?;
                if *inline > 0 {
                    write!(f, ", {inline} inline dicts (dropped by pdf_oxide)")?;
                }
                Ok(())
            }
            Diagnostic::UnparsedAnnots { dropped, total } => write!(
                f,
                "{dropped} of {total} annotations in /Annots were not parsed \
                 (rerun with --stats)"
            ),
            Diagnostic::AnnotsUnreadable { page, error } => {
                write!(
                    f,
                    "page {page}: /Annots could not be read ({error}); page skipped"
                )
            }
            Diagnostic::NoMediaBox { page, annotations } => write!(
                f,
                "page {page}: no /MediaBox, and none inherited; {annotations} \
                 annotation(s) here cannot be placed and are skipped"
            ),
            Diagnostic::TextUnextractable { page, error } => write!(
                f,
                "page {page}: text could not be extracted ({error}); markup here \
                 is reported with its comment but no covered text"
            ),
            Diagnostic::UnmatchedQuads {
                page,
                unmatched,
                total,
                rotation,
            } => {
                write!(
                    f,
                    "page {page}: {unmatched}/{total} quads matched no glyphs"
                )?;
                if rotation % 360 != 0 {
                    write!(f, "; page carries /Rotate {rotation}")?;
                }
                write!(f, " (try --min-overlap or --geometry)")
            }
            Diagnostic::EncodingSniffed { count } => write!(
                f,
                "{count} string(s) carried no byte order mark and were read as \
                 UTF-8 rather than PDFDocEncoding"
            ),
            Diagnostic::UndefinedPdfDocBytes { count } => write!(
                f,
                "{count} byte(s) fell on PDFDocEncoding slots the spec leaves \
                 undefined (0x7F, 0x9F, 0xAD) and were read as Latin-1"
            ),
            Diagnostic::DeclarationOutsideText { page, text } => write!(
                f,
                "page {page}: declaration {text:?} opens outside BT/ET; text suppressed"
            ),
            Diagnostic::DeclarationCoversNoText { page, text } => write!(
                f,
                "page {page}: declaration {text:?} covers no shown text; text suppressed"
            ),
            Diagnostic::DeclarationUnanchored { page, text } => write!(
                f,
                "page {page}: declaration {text:?} covers no shown text; nothing to \
                 place it against"
            ),
            Diagnostic::DeclarationUnrepaired {
                page,
                text,
                found,
                wanted,
                x,
            } => write!(
                f,
                "page {page}: declaration {text:?}: found {found} of {wanted} \
                 replacement characters at x={x:.2}; text suppressed"
            ),
            Diagnostic::ScopesLeftOpen { page, count } => {
                write!(f, "page {page}: {count} marked-content scope(s) left open")
            }
        }
    }
}

/// Everything collected while reading one document, in the order it was found.
///
/// Insertion order is preserved rather than sorted by severity. It is already
/// page order, and a warning about page 7 reads better beside the `--stats`
/// line for page 7 than in a block of warnings at the end.
#[derive(Default, Debug)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub(crate) fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    /// Has this one already been raised?
    ///
    /// `GeometryAssumed` is a fact about the document reported at the first
    /// place it can do damage, which may be any of many pages, so the caller
    /// needs to ask rather than track a flag of its own.
    pub(crate) fn contains(&self, d: &Diagnostic) -> bool {
        self.items.contains(d)
    }

    pub(crate) fn warnings(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity() == Severity::Warning)
            .count()
    }

    /// The whole set as it goes to stderr, one line each, prefix included.
    ///
    /// A `String` rather than a `Write`, so the caller can send it through
    /// [`crate::write_to`] and inherit its handling of a closed pipe. Empty
    /// when there is nothing to say, so the caller can skip the write.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        for d in &self.items {
            out.push_str(d.severity().prefix());
            out.push_str(&d.to_string());
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------- wording
    // These pin the claim in `Display`'s doc comment: unifying the channel
    // changed the plumbing, not what the user reads. Each expectation was
    // taken from the `eprintln!` the variant replaced.

    #[test]
    fn reproduces_the_messages_it_replaced() {
        assert_eq!(
            Diagnostic::GeometryAssumed {
                space: GlyphSpace::BottomUp
            }
            .to_string(),
            "too little text to measure the glyph coordinate convention; \
             assumed BottomUp. Set --geometry explicitly."
        );
        assert_eq!(
            Diagnostic::UnparsedAnnots {
                dropped: 3,
                total: 20
            }
            .to_string(),
            "3 of 20 annotations in /Annots were not parsed (rerun with --stats)"
        );
        assert_eq!(
            Diagnostic::EncodingSniffed { count: 2 }.to_string(),
            "2 string(s) carried no byte order mark and were read as UTF-8 \
             rather than PDFDocEncoding"
        );
        assert_eq!(
            Diagnostic::UndefinedPdfDocBytes { count: 1 }.to_string(),
            "1 byte(s) fell on PDFDocEncoding slots the spec leaves undefined \
             (0x7F, 0x9F, 0xAD) and were read as Latin-1"
        );
    }

    #[test]
    fn stats_lines_keep_their_alignment_and_gain_no_prefix() {
        // The page number was right-aligned to three columns so that a
        // multi-page run reads as a table, and the line is an answer to
        // --stats rather than a complaint, so it carries no `note:`.
        let d = Diagnostic::PageAnnotCounts {
            page: 7,
            in_annots: 5,
            parsed: 4,
            inline: 0,
        };
        assert_eq!(d.to_string(), "page   7: 5 in /Annots, 4 parsed");
        assert_eq!(d.severity(), Severity::Info);
        assert_eq!(d.severity().prefix(), "");
    }

    #[test]
    fn inline_dictionaries_are_mentioned_only_when_present() {
        assert_eq!(
            Diagnostic::PageAnnotCounts {
                page: 1,
                in_annots: 5,
                parsed: 4,
                inline: 1,
            }
            .to_string(),
            "page   1: 5 in /Annots, 4 parsed, 1 inline dicts (dropped by pdf_oxide)"
        );
    }

    #[test]
    fn rotation_is_named_only_when_the_page_carries_one() {
        let plain = Diagnostic::UnmatchedQuads {
            page: 2,
            unmatched: 1,
            total: 4,
            rotation: 0,
        };
        assert_eq!(
            plain.to_string(),
            "page 2: 1/4 quads matched no glyphs (try --min-overlap or --geometry)"
        );
        let rotated = Diagnostic::UnmatchedQuads {
            page: 3,
            unmatched: 11,
            total: 11,
            rotation: 90,
        };
        assert_eq!(
            rotated.to_string(),
            "page 3: 11/11 quads matched no glyphs; page carries /Rotate 90 \
             (try --min-overlap or --geometry)"
        );
    }

    #[test]
    fn a_full_turn_is_not_a_rotation() {
        // 360 and -360 display as unrotated, matching the `% 360` test the
        // extractor uses. A page can legally carry /Rotate 360.
        for r in [360, -360] {
            let d = Diagnostic::UnmatchedQuads {
                page: 1,
                unmatched: 1,
                total: 1,
                rotation: r,
            };
            assert!(!d.to_string().contains("/Rotate"), "{r} leaked");
        }
    }

    #[test]
    fn a_negative_rotation_is_reported_as_written() {
        // /Rotate -90 is legal and the `page/` family carries one. Reporting
        // it normalised to 270 would send the reader looking for a value the
        // file does not contain.
        assert!(
            Diagnostic::UnmatchedQuads {
                page: 1,
                unmatched: 1,
                total: 1,
                rotation: -90,
            }
            .to_string()
            .contains("/Rotate -90")
        );
    }

    // ------------------------------------------------------------ severity

    #[test]
    fn only_warnings_count_toward_strict() {
        let mut d = Diagnostics::default();
        d.push(Diagnostic::PageAnnotCounts {
            page: 1,
            in_annots: 1,
            parsed: 1,
            inline: 0,
        });
        d.push(Diagnostic::EncodingSniffed { count: 1 });
        assert_eq!(d.warnings(), 0);

        d.push(Diagnostic::NoMediaBox {
            page: 2,
            annotations: 3,
        });
        assert_eq!(d.warnings(), 1);
    }

    #[test]
    fn severity_is_ordered_so_strict_can_be_a_threshold() {
        assert!(Severity::Warning > Severity::Note);
        assert!(Severity::Note > Severity::Info);
    }

    // -------------------------------------------------------------- render

    #[test]
    fn nothing_to_say_renders_to_nothing() {
        // So the caller can skip the write entirely rather than emitting a
        // blank line to stderr on a clean document.
        assert_eq!(Diagnostics::default().render(), "");
    }

    #[test]
    fn renders_in_insertion_order_with_prefixes() {
        let mut d = Diagnostics::default();
        d.push(Diagnostic::PageAnnotCounts {
            page: 1,
            in_annots: 2,
            parsed: 1,
            inline: 0,
        });
        d.push(Diagnostic::UnmatchedQuads {
            page: 1,
            unmatched: 1,
            total: 2,
            rotation: 0,
        });
        d.push(Diagnostic::EncodingSniffed { count: 1 });
        assert_eq!(
            d.render(),
            "page   1: 2 in /Annots, 1 parsed\n\
             warning: page 1: 1/2 quads matched no glyphs (try --min-overlap or --geometry)\n\
             note: 1 string(s) carried no byte order mark and were read as UTF-8 \
             rather than PDFDocEncoding\n"
        );
    }

    #[test]
    fn the_actual_text_messages_survived_being_typed() {
        // Taken from the `format!` calls in `actual_text` that these replaced.
        // The page prefix used to be added by the caller wrapping a string;
        // it is now part of the variant, so the line must come out the same.
        assert_eq!(
            Diagnostic::DeclarationOutsideText {
                page: 2,
                text: "Figure 3".to_string(),
            }
            .to_string(),
            "page 2: declaration \"Figure 3\" opens outside BT/ET; text suppressed"
        );
        assert_eq!(
            Diagnostic::DeclarationCoversNoText {
                page: 1,
                text: "A".to_string(),
            }
            .to_string(),
            "page 1: declaration \"A\" covers no shown text; text suppressed"
        );
        assert_eq!(
            Diagnostic::ScopesLeftOpen { page: 4, count: 2 }.to_string(),
            "page 4: 2 marked-content scope(s) left open"
        );
    }

    #[test]
    fn an_unrepaired_declaration_names_the_text_it_withheld() {
        // The one diagnostic that reports missing output rather than doubtful
        // output, so it has to say which string went missing and where.
        let d = Diagnostic::DeclarationUnrepaired {
            page: 1,
            text: "\u{2122}".to_string(),
            found: 1,
            wanted: 3,
            x: 72.5,
        };
        assert_eq!(
            d.to_string(),
            "page 1: declaration \"™\": found 1 of 3 replacement characters \
             at x=72.50; text suppressed"
        );
        assert_eq!(d.severity(), Severity::Warning);
    }

    // --------------------------------------------------------------- scope

    #[test]
    fn the_two_item_level_facts_ride_on_the_record() {
        // The two that describe one item: a selection whose quads found no
        // glyphs, and one that may contain text the document disowned.
        assert!(
            Diagnostic::UnmatchedQuads {
                page: 1,
                unmatched: 1,
                total: 2,
                rotation: 0,
            }
            .is_annotation_scope()
        );
        assert!(
            Diagnostic::DeclarationUnrepaired {
                page: 1,
                text: "x".to_string(),
                found: 0,
                wanted: 1,
                x: 0.0,
            }
            .is_annotation_scope()
        );
        // Document scope: these describe the page, the content stream or the
        // document, and attaching one to an item would assert a link nothing
        // has established.
        for d in [
            Diagnostic::GeometryAssumed {
                space: GlyphSpace::BottomUp,
            },
            Diagnostic::EncodingSniffed { count: 1 },
            Diagnostic::ScopesLeftOpen { page: 1, count: 1 },
            Diagnostic::DeclarationUnanchored {
                page: 1,
                text: "x".to_string(),
            },
        ] {
            assert!(!d.is_annotation_scope(), "{d:?} claimed annotation scope");
        }
    }

    // --------------------------------------------------------- the wire

    #[test]
    fn serialises_internally_tagged_in_snake_case() {
        let json = serde_json::to_string(&Diagnostic::UnmatchedQuads {
            page: 3,
            unmatched: 2,
            total: 5,
            rotation: 90,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"kind":"unmatched_quads","page":3,"unmatched":2,"total":5,"rotation":90}"#
        );
    }

    #[test]
    fn the_wire_form_carries_no_rendered_message() {
        // Deliberate: prose is for stderr. Putting it in the JSON invites
        // consumers to parse it, and then the wording cannot be changed.
        let json = serde_json::to_string(&Diagnostic::DeclarationUnrepaired {
            page: 1,
            text: "\u{2122}".to_string(),
            found: 1,
            wanted: 3,
            x: 72.5,
        })
        .unwrap();
        assert!(!json.contains("suppressed"), "{json}");
        assert!(
            json.contains(r#""kind":"declaration_unrepaired""#),
            "{json}"
        );
    }

    #[test]
    fn contains_lets_a_document_level_warning_be_raised_once() {
        let mut d = Diagnostics::default();
        let assumed = Diagnostic::GeometryAssumed {
            space: GlyphSpace::BottomUp,
        };
        assert!(!d.contains(&assumed));
        d.push(assumed.clone());
        assert!(d.contains(&assumed));
        // A different convention is a different fact, not a duplicate.
        assert!(!d.contains(&Diagnostic::GeometryAssumed {
            space: GlyphSpace::TopDownTop
        }));
    }
}
