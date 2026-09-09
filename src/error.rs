//! The four ways a report fails to be produced.
//!
//! Everything else that goes wrong is a [`crate::diagnostic::Diagnostic`]: a
//! page skipped, an encoding assumed, a declaration withheld. Those degrade
//! the report. These four mean there is no report, and the process exits 1.
//!
//! # Why not `Box<dyn Error>`
//!
//! That was the previous signature, and it is the right choice for an
//! application whose errors only ever get printed. It is the wrong one here
//! for two reasons. It is not `Send + Sync`, so it cannot cross a thread or
//! sit in a `Result` a library caller holds; and it cannot be matched on, so
//! the caller cannot tell "this file is not a PDF" from "the disk is full"
//! without parsing the message. `extract` will be a library function shortly,
//! and by then the type is part of the contract.
//!
//! # Why the sources are boxed rather than named
//!
//! `Open` and `PageCount` both wrap a `pdf_oxide::Error`, and naming it here
//! would put it in this crate's public API. That matters because the
//! dependency is pinned to `=0.3.77` on purpose — `actual_text::repair`
//! depends on the shape of a bug in that exact version — so unpinning it one
//! day should not be a breaking change to *our* signature. Boxing keeps the
//! message, keeps [`std::error::Error::source`] working, and still allows
//! `downcast_ref::<pdf_oxide::Error>()` for anyone who genuinely wants it.
//! Nothing is given up except the coupling.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// A boxed cause, so no dependency's error type appears in our signatures.
pub type Source = Box<dyn StdError + Send + Sync + 'static>;

/// Anything that stops a report existing at all.
#[derive(Debug)]
pub enum Error {
    /// The file could not be read.
    ///
    /// Belongs to the caller, not to `extract`, which takes bytes. Split out
    /// of a variant that used to mean this *and* `Parse`, because "no such
    /// file" and "not a PDF" want different reactions and arrived
    /// indistinguishable.
    Read { path: PathBuf, source: io::Error },

    /// The bytes are not a PDF this parser accepts.
    Parse { source: Source },

    /// The document opened, but its page tree could not be counted.
    ///
    /// Separate from `Open` because it means something different: the file is
    /// a PDF and the header parsed, but the catalogue or the page tree is
    /// broken. Nothing can be extracted without a page count, so unlike a
    /// single unreadable page this is fatal.
    PageCount { source: Source },

    /// The records could not be rendered as JSON.
    ///
    /// Effectively unreachable for our data. The report is strings, integers
    /// and `f64`s, and serde_json refuses only a map key that is not a string,
    /// of which there are none. It exists so that `to_string_pretty`'s
    /// `Result` is propagated rather than unwrapped, because "effectively" is
    /// doing some work in that sentence.
    ///
    /// Worth recording what does *not* land here: a NaN in `/Rect`. serde_json
    /// writes NaN and infinity as `null` rather than failing, so a malformed
    /// rect becomes `"rect": [null, 700.0, ...]` with no error and no
    /// diagnostic — and `sort_records` compares it with `partial_cmp(...)
    /// .unwrap_or(Equal)`, so it quietly loses its place in reading order too.
    /// That gap belongs to whatever reads the rect, not here.
    Serialize { source: Source },

    /// The report could not be written out.
    ///
    /// A closed pipe is not one of these. `tool paper.pdf | head` is a normal
    /// way to run a filter, and [`crate::write_to`] treats `BrokenPipe` as
    /// success; this is a full disk, a bad redirect, a revoked permission.
    Write { source: io::Error },
}

impl Error {
    pub fn read(path: &Path, source: io::Error) -> Self {
        Error::Read {
            path: path.to_path_buf(),
            source,
        }
    }

    pub fn parse(source: impl Into<Source>) -> Self {
        Error::Parse {
            source: source.into(),
        }
    }

    pub fn page_count(source: impl Into<Source>) -> Self {
        Error::PageCount {
            source: source.into(),
        }
    }
}

impl fmt::Display for Error {
    /// The message says what could not be done, not what went wrong.
    ///
    /// The cause is on `source`, and `main` prints the chain, so repeating it
    /// here would print it twice.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read { path, .. } => write!(f, "could not read {}", path.display()),
            Error::Parse { .. } => write!(f, "could not parse the document as a PDF"),
            Error::PageCount { .. } => write!(f, "could not count the pages of the document"),
            Error::Serialize { .. } => write!(f, "could not render the report as JSON"),
            Error::Write { .. } => write!(f, "could not write the report"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Parse { source } | Error::PageCount { source } | Error::Serialize { source } => {
                Some(source.as_ref())
            }
            Error::Read { source, .. } => Some(source),
            Error::Write { source } => Some(source),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Error::Serialize {
            source: Box::new(source),
        }
    }
}

impl From<io::Error> for Error {
    /// Every bare `io::Error` in this crate comes from writing the report.
    /// Opening the document goes through pdf_oxide, which wraps its own io in
    /// `pdf_oxide::Error`, so it arrives as `Open` instead.
    fn from(source: io::Error) -> Self {
        Error::Write { source }
    }
}

/// Render an error and everything that caused it, one line each.
///
/// Without this the useful half is invisible: `could not read paper.pdf` says
/// nothing, while the cause underneath it is `Failed to parse object at byte
/// 12043`, which is the line someone can act on.
pub fn report(e: &dyn StdError) -> String {
    let mut out = format!("error: {e}\n");
    let mut cause = e.source();
    while let Some(c) = cause {
        out.push_str(&format!("  caused by: {c}\n"));
        cause = c.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An error with a cause of its own, to check the chain is walked and not
    /// just the head.
    #[derive(Debug)]
    struct Inner;
    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "at byte 12043")
        }
    }
    impl StdError for Inner {}

    #[derive(Debug)]
    struct Outer;
    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "malformed object")
        }
    }
    impl StdError for Outer {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&Inner)
        }
    }

    #[test]
    fn names_the_file_it_could_not_read() {
        // The path is the first thing anyone needs, and `Box<dyn Error>` from
        // pdf_oxide never carried it — the old message was just the parse
        // failure, with no way to tell which file produced it when several
        // were being processed.
        let e = Error::read(
            Path::new("/tmp/paper.pdf"),
            io::Error::new(io::ErrorKind::NotFound, "no such file"),
        );
        assert_eq!(e.to_string(), "could not read /tmp/paper.pdf");
    }

    #[test]
    fn the_message_does_not_repeat_the_cause() {
        let e = Error::parse(Outer);
        assert!(!e.to_string().contains("malformed"));
        assert_eq!(
            e.source().map(ToString::to_string).as_deref(),
            Some("malformed object")
        );
    }

    #[test]
    fn report_walks_the_whole_chain() {
        let e = Error::parse(Outer);
        assert_eq!(
            report(&e),
            "error: could not parse the document as a PDF\n  \
             caused by: malformed object\n  \
             caused by: at byte 12043\n"
        );
    }

    #[test]
    fn a_missing_file_is_not_a_malformed_one() {
        // These were one variant while `PdfDocument::open` did both jobs, so
        // a typo in a filename and a corrupt PDF produced the same message.
        // `extract` takes bytes now, which forces them apart.
        let missing = Error::read(
            Path::new("nope.pdf"),
            io::Error::new(io::ErrorKind::NotFound, "no such file"),
        );
        let corrupt = Error::parse(Outer);
        assert!(missing.to_string().contains("nope.pdf"));
        assert!(!corrupt.to_string().contains("read"));
        assert!(matches!(missing, Error::Read { .. }));
        assert!(matches!(corrupt, Error::Parse { .. }));
    }

    #[test]
    fn a_broken_page_tree_is_not_reported_as_an_unreadable_file() {
        // These used to be the same `?` on the same `Box<dyn Error>`, so a
        // document that opened fine but had a broken catalogue produced a
        // message blaming the file.
        let e = Error::page_count(Outer);
        assert_eq!(e.to_string(), "could not count the pages of the document");
    }

    #[test]
    fn io_and_serde_convert_so_the_question_mark_still_works() {
        let io_err: Error = io::Error::new(io::ErrorKind::PermissionDenied, "denied").into();
        assert!(matches!(io_err, Error::Write { .. }));
        assert_eq!(io_err.to_string(), "could not write the report");

        // A real serialisation failure, not a parse one: serde_json rejects a
        // map key that is not a string, and `()` is not.
        let map = std::collections::HashMap::from([((), 1i32)]);
        let json_err: Error = serde_json::to_string(&map).unwrap_err().into();
        assert!(matches!(json_err, Error::Serialize { .. }));
        assert_eq!(json_err.to_string(), "could not render the report as JSON");
    }

    #[test]
    fn the_original_error_is_still_reachable() {
        // The point of boxing rather than discarding: a caller who does want
        // the concrete type can still get at it.
        let e = Error::page_count(io::Error::new(io::ErrorKind::UnexpectedEof, "eof"));
        let src = e.source().expect("a source");
        assert!(src.downcast_ref::<io::Error>().is_some());
    }

    #[test]
    fn the_error_type_crosses_threads() {
        // What `Box<dyn Error>` could not do, and the reason this exists at
        // all rather than a type alias.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Error>();
    }
}
