//! Everything between "here are some bytes" and "here is some text".
//!
//! Separated from `main.rs` so the only thing the UI does is move strings
//! around: no `Options` construction, no serialisation, no error formatting
//! inside a `view!`. It is also the file to read to see how much of the
//! library a consumer actually has to touch — which is the second reason the
//! playground lives in this repository at all.

use pdf_annotation_extractor::{
    Descriptor, Numbering, Options, Report, Severity, Style, error, extract, markdown,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Json,
}

/// The CLI's flags, as far as they mean anything in a browser.
///
/// `--debug-geometry` and `--debug-quads` are absent: they write a per-glyph
/// trace to stderr, which here is the developer console of whoever opened the
/// page. `--strict` is absent because there is no exit code to set.
#[derive(Clone, PartialEq)]
pub struct Params {
    pub format: Format,
    pub keep_hyphens: bool,
    pub keep_ligatures: bool,
    pub split_quads: bool,
    pub keep_empty: bool,
    pub page_counts: bool,
    pub min_overlap: f32,
    pub space_gap: f32,
    pub numbering: Numbering,
    pub descriptors: Vec<Descriptor>,
}

impl Default for Params {
    fn default() -> Self {
        let defaults = Options::default();
        Params {
            format: Format::Markdown,
            keep_hyphens: defaults.keep_hyphens,
            keep_ligatures: defaults.keep_ligatures,
            split_quads: defaults.split_quads,
            keep_empty: defaults.keep_empty,
            page_counts: defaults.page_counts,
            min_overlap: defaults.min_overlap,
            space_gap: defaults.space_gap,
            numbering: Numbering::None,
            descriptors: Vec::new(),
        }
    }
}

/// One rendered diagnostic line, with the severity recovered for colouring.
#[derive(Clone)]
pub struct Note {
    pub severity: Severity,
    pub text: String,
}

#[derive(Clone)]
pub struct Outcome {
    /// The report, in whichever format was asked for.
    pub text: String,
    /// What had to be assumed or given up on, document-scope and per-item.
    pub notes: Vec<Note>,
    pub annotations: usize,
    pub warnings: usize,
}

impl Outcome {
    /// Suggested filename for the download button.
    pub fn file_name(&self, source: &str, format: Format) -> String {
        let stem = source.strip_suffix(".pdf").unwrap_or(source);
        match format {
            Format::Markdown => format!("{stem}.md"),
            Format::Json => format!("{stem}.json"),
        }
    }
}

pub fn run(bytes: Vec<u8>, name: &str, params: &Params) -> Result<Outcome, String> {
    let mut options = Options::default();
    options.document_name = Some(name.to_string());
    options.keep_hyphens = params.keep_hyphens;
    options.keep_ligatures = params.keep_ligatures;
    options.split_quads = params.split_quads;
    options.keep_empty = params.keep_empty;
    options.page_counts = params.page_counts;
    options.min_overlap = params.min_overlap;
    options.space_gap = params.space_gap;

    let report = extract(bytes, &options).map_err(|e| error::report(&e))?;

    let text = match params.format {
        Format::Markdown => {
            let style = Style {
                numbering: params.numbering,
                descriptors: params.descriptors.clone(),
                ..Style::default()
            };
            markdown::render(&report.annotations, &style)
        }
        // A bare array, byte for byte what `--format json` writes, so anything
        // built against the CLI's output can be tried here first.
        Format::Json => serde_json::to_string_pretty(&report.annotations)
            .map_err(|e| format!("could not serialise the report: {e}"))?,
    };

    Ok(Outcome {
        annotations: report.annotations.len(),
        warnings: report.diagnostics.warnings(),
        notes: notes(&report),
        text,
    })
}

/// Collect the diagnostics the way the CLI shows them on stderr.
///
/// Both scopes arrive typed — `Diagnostics::iter` for the document,
/// `Record::diagnostics` for each item — so `severity` is asked for rather
/// than recovered from the `note: ` prefix `render` would have written.
fn notes(report: &Report) -> Vec<Note> {
    let document = report.diagnostics.iter().map(|d| Note {
        severity: d.severity(),
        text: d.to_string(),
    });

    // Prefixed with the page so a reader can find the item it belongs to.
    let annotations = report.annotations.iter().flat_map(|record| {
        record.diagnostics.iter().map(move |d| Note {
            severity: d.severity(),
            text: format!("page {}: {d}", record.page),
        })
    });

    document.chain(annotations).collect()
}
