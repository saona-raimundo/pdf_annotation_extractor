//! Command line front end.
//!
//! Argument parsing, output formatting and exit codes. Everything that reads a
//! PDF lives in the library, so that the fixture harness and a browser build
//! can reach it without going through a process.

#![forbid(unsafe_code)]

use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

use pdf_annotation_extractor::{
    Descriptor, Error, GlyphSpace, Numbering, Options, Style, error, extract, markdown,
};

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

    /// Exit non-zero if anything was assumed, skipped or possibly wrong.
    /// For a pipeline that would rather stop than accept a partial report.
    #[arg(long)]
    strict: bool,

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

/// What `run` decided the process should exit with.
///
/// Exit codes: 0 clean, 1 could not produce a report at all, 3 produced one
/// but `--strict` was set and something was assumed or skipped. 2 is left
/// alone because clap uses it for a usage error, and a caller distinguishing
/// "you typed it wrong" from "the document is dubious" should not have to
/// guess which it got.
enum Outcome {
    Clean,
    StrictWarnings(usize),
}

fn main() {
    match run() {
        Ok(Outcome::Clean) => {}
        Ok(Outcome::StrictWarnings(n)) => {
            eprintln!("error: {n} warning(s) and --strict was set");
            std::process::exit(3);
        }
        Err(e) => {
            // The chain, not just the head: "could not read paper.pdf" is not
            // actionable on its own, and the cause under it is the byte offset
            // that is.
            eprint!("{}", error::report(&e));
            std::process::exit(1);
        }
    }
}

impl From<&Args> for Options {
    // `Options` is `#[non_exhaustive]`, so a struct literal is not available
    // from outside the library — which is the point of it, and this is its
    // first real consumer. Default-then-assign is the documented pattern, and
    // clippy's advice to use a literal instead does not apply when the literal
    // is forbidden.
    #[allow(clippy::field_reassign_with_default)]
    fn from(args: &Args) -> Self {
        let mut o = Options::default();
        o.min_overlap = args.min_overlap;
        o.space_gap = args.space_gap;
        o.geometry = match args.geometry {
            GeometryArg::Auto => None,
            GeometryArg::TopDownTop => Some(GlyphSpace::TopDownTop),
            GeometryArg::TopDownBottom => Some(GlyphSpace::TopDownBottom),
            GeometryArg::BottomUp => Some(GlyphSpace::BottomUp),
        };
        o.keep_hyphens = args.keep_hyphens;
        o.keep_ligatures = args.keep_ligatures;
        o.split_quads = args.split_quads;
        o.keep_empty = args.keep_empty;
        o.document_name = args
            .file
            .file_name()
            .map(|s| s.to_string_lossy().to_string());
        o.page_counts = args.stats;
        o.debug_geometry = args.debug_geometry;
        o.debug_quads = args.debug_quads;
        o
    }
}

fn run() -> Result<Outcome, Error> {
    let args = Args::parse();
    let opts = Options::from(&args);

    // Read the file here rather than inside `extract`, which is the whole
    // point of it taking bytes. It also splits a failure that used to be one:
    // a missing file and a malformed one arrived as the same error.
    let bytes = std::fs::read(&args.file).map_err(|e| Error::read(&args.file, e))?;
    let report = extract(bytes, &opts)?;

    let text = match args.format {
        Format::Json => format!("{}\n", serde_json::to_string_pretty(&report.annotations)?),
        Format::Markdown => markdown::render(
            &report.annotations,
            &Style {
                descriptors: args.show.iter().copied().map(Descriptor::from).collect(),
                numbering: args.number.into(),
                ..Style::default()
            },
        ),
    };

    // Before the report, not after: under `tool paper.pdf | head` the write
    // to stdout may return early on a closed pipe, and a warning the reader
    // never sees is the failure mode the diagnostics exist to remove.
    let notes = report.diagnostics.render();
    if !notes.is_empty() {
        write_to(io::stderr().lock(), &notes)?;
    }

    write_out(&text)?;

    let warnings = report.diagnostics.warnings();
    Ok(if args.strict && warnings > 0 {
        Outcome::StrictWarnings(warnings)
    } else {
        Outcome::Clean
    })
}

/// Write the report to the writer, treating a closed pipe as success.
///
/// Here rather than in the library: "a closed pipe means the reader has what
/// it wanted" is a convention of Unix filters, not a fact about PDFs, and a
/// library has no business holding an opinion about stdout.
///
/// `println!` panics if stdout is gone, which is what happens under
/// `tool paper.pdf | head`. It goes unnoticed on small reports because a
/// single write below the 64 KiB pipe buffer completes before the reader
/// exits; past that it aborts with exit 101 and a panic trace. Every other
/// Unix filter exits quietly instead.
fn write_to<W: Write>(mut out: W, text: &str) -> Result<(), Error> {
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn write_out(text: &str) -> Result<(), Error> {
    write_to(io::stdout().lock(), text)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
