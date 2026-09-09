//! A tripwire on the upstream defect `actual_text` exists to work around.
//!
//! `actual_text::repair` does not merely tolerate a pdf_oxide bug, it depends
//! on the bug's exact shape: `/ActualText` is re-encoded through the font in
//! character mode, so the declared string arrives as one glyph per UTF-8 byte
//! at successive cursor positions, and `repair` walks that chain to remove it.
//!
//! If the defect is fixed, the chain is not there to find. Every declaration
//! fails `chain.len() != wanted`, and its text is *suppressed* — so a fixed
//! dependency makes our output worse, quietly, on any `cargo update` that a
//! caret range would have allowed. `Cargo.toml` pins the version; this test
//! says why, and fails when the assumption stops holding.
//!
//! # Why it keys on `™` and not on the other two declarations
//!
//! `actualtext-min.pdf` declares three spans. Two of them — `Section` and
//! `Figure 3` — are pure ASCII, and the font is Helvetica with
//! `/WinAnsiEncoding`, so feeding their UTF-8 bytes back through the font
//! returns the same characters. For those the defect corrupts only the
//! *geometry*: the replacement is typeset with its own metrics from the span
//! origin and overruns the following word. The text is right.
//!
//! `™` is U+2122, three bytes in UTF-8, and none of them is the character. It
//! is the only one of the three where the defect is visible in the characters
//! alone, which is what makes it the thing to assert on.

use std::path::PathBuf;

use pdf_oxide::PdfDocument;

/// The declared string that cannot survive the round trip through the font.
const DECLARED: char = '\u{2122}';

fn repro() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("escalations/pdf_oxide/1 actual_text/actualtext-min.pdf")
}

#[test]
fn character_mode_still_re_encodes_actual_text() {
    let path = repro();
    let doc =
        PdfDocument::open(&path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));

    // Guard the fixture itself. Span mode reads the key correctly, so if the
    // declared string is missing here it is the repro or the API that changed,
    // not the defect that was fixed — and the assertion below would then pass
    // for the wrong reason.
    let text = doc
        .extract_text(0)
        .unwrap_or_else(|e| panic!("extract_text failed: {e}"));
    assert!(
        text.contains(DECLARED),
        "extract_text no longer reports the declared {DECLARED:?}. The repro or \
         the pdf_oxide API has changed, so the canary below proves nothing. \
         Fix this before reading it.\n\
         got: {text:?}"
    );

    // The defect. Character mode should still fail to produce the declared
    // character, because it re-encodes the already-decoded string through the
    // font.
    let chars = doc
        .extract_chars(0)
        .unwrap_or_else(|e| panic!("extract_chars failed: {e}"));
    let recovered: String = chars.iter().map(|c| c.char).collect();

    assert!(
        !chars.iter().any(|c| c.char == DECLARED),
        "pdf_oxide now reports /ActualText correctly in character mode.\n\n\
         This is good news and a breaking change for us. `actual_text::repair` \
         walks a cursor chain of re-encoded bytes that no longer exists, so \
         every declaration in every document now fails its length check and \
         has its text SUPPRESSED. Do not just delete this test.\n\n\
         What to do:\n  \
         1. Verify against the `actual_text/` fixture family, which is the \
            real evidence — this file is one page of one PDF.\n  \
         2. Give `repair` a path for an already-correct glyph list, or remove \
            it and keep `scan`: the atomicity rule and the empty declaration \
            are ours to apply either way.\n  \
         3. Unpin pdf_oxide in Cargo.toml and delete the comment above the \
            dependency.\n\n\
         extract_chars gave: {recovered:?}"
    );
}
