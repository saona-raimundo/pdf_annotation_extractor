//! Fixture tests: extract from real annotated PDFs and compare against each
//! fixture family's `expected.json`.
//!
//! Calls the library rather than spawning the binary. That is not only faster:
//! a subprocess hands back stdout, so the diagnostics went to fd 2 and nothing
//! here could see them. Half of what extraction knows — a page skipped, an
//! encoding assumed, a declaration withheld — was untestable for as long as
//! this went through a process boundary.
//!
//! `expected.json` states what the *document* means. It is never adjusted to
//! match a producer or to match us, and nothing here is allowlisted: every
//! item is checked on every run. An item we cannot yet extract fails, and the
//! suite is red until it is fixed. Why each open failure is open belongs in
//! the family's PRODUCERS.md escalation log, which is the single record of
//! what is unfinished.
//!
//! Layout is discovered: every directory under `tests/` containing an
//! `expected.json` is a fixture family, and every `*.pdf` in its `producers/`
//! subdirectory is checked against it.
//!
//!   tests/basic/expected.json
//!   tests/basic/producers/base-papers-50.2.pdf
//!
//! Adding a producer, or a whole family, needs no change to this file.
//! (Cargo ignores `tests/basic/` as a build target: it has no `main.rs`.)
//!
//! What the corpus reports is now asserted, not printed: see
//! `no_fixture_reports_a_warning`, and `the_diagnostic_channel_is_wired`,
//! which is the reason the first is worth anything.
//!
//! Pairing. An expectation is bound to the record that claims it by its
//! sentinel where it has one, and by page + subtype + comment where it does
//! not. Both kinds still assert `covered_text`: see `matched_by_sentinel`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use pdf_annotation_extractor::{Options, Record, Report};

// --------------------------------------------------------------- expected.json

#[derive(Deserialize)]
struct Expected {
    pages_without_annotations: Vec<usize>,
    annotations: Vec<ExpectedAnnot>,
}

#[derive(Deserialize)]
struct ExpectedAnnot {
    id: String,
    page: usize,
    /// PDF subtype name: Highlight, Underline, StrikeOut, Squiggly, Text,
    /// FreeText, Ink, Square. Mapped to our `kind` by `kind_for`.
    subtype: String,
    /// What the selection covers, `null` when the record has none.
    ///
    /// Three states, and they are not interchangeable:
    ///
    /// * carries this item's own `[[id]]` — paired by sentinel, text compared
    /// * set but carries no sentinel — paired by comment, text still compared.
    ///   For annotations that cannot hold a distinct sentinel because they
    ///   cover the same words as another (tests/inventory IN1).
    /// * `null` — paired by comment, and the record must have no covered text
    ///   either. For annotations with no quads: notes, replies, FreeText.
    covered_text: Option<String>,
    comment: Option<String>,
    /// null means "any value": producers commonly substitute the system user
    author: Option<String>,
    /// "undefined" compares as a character multiset instead of a string, for
    /// items with no canonical linearisation (stacked mathematical limits).
    /// Absent or anything else means reading order, compared exactly.
    #[serde(default)]
    order: Option<String>,
}

// ------------------------------------------------------------------- skips

/// Items a producer's file fails to record, read from `tests/<family>/skips.json`.
///
/// This is an inventory of the *input*, not an expectation. It says a file does
/// not contain a usable recording of an item, so nothing is asserted about it.
/// It never changes what the correct answer is: that stays in expected.json.
#[derive(Deserialize, Default)]
struct Skips {
    #[serde(default)]
    producers: BTreeMap<String, BTreeMap<String, SkipEntry>>,
}

#[derive(Deserialize)]
struct SkipEntry {
    reason: String,
}

// ------------------------------------------------------------------- discovery

struct Family {
    name: String,
    expected: Expected,
    producers: Vec<PathBuf>,
    skips: Skips,
}

fn tests_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn families() -> Vec<Family> {
    let root = tests_dir();
    let mut out = Vec::new();

    let entries =
        std::fs::read_dir(&root).unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()));

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let expected_path = dir.join("expected.json");
        if !expected_path.exists() {
            continue;
        }

        let bytes = std::fs::read(&expected_path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", expected_path.display()));
        let expected: Expected = serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("{} did not parse: {e}", expected_path.display()));
        validate(&expected, &expected_path);

        let mut producers: Vec<PathBuf> = std::fs::read_dir(dir.join("producers"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "pdf"))
                    .collect()
            })
            .unwrap_or_default();
        producers.sort();

        assert!(
            !producers.is_empty(),
            "fixture family {} has expected.json but no producers/*.pdf",
            dir.display()
        );

        // Optional: absent means nothing is skipped.
        let skips: Skips = match std::fs::read(dir.join("skips.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("{}/skips.json did not parse: {e}", dir.display())),
            Err(_) => Skips::default(),
        };

        out.push(Family {
            name: dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            expected,
            producers,
            skips,
        });
    }

    assert!(
        !out.is_empty(),
        "no fixture families found under {}",
        root.display()
    );
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// --------------------------------------------------------------------- helpers

/// Extract from one fixture, or fail the test naming the file.
///
/// Default options throughout. A fixture that only passes under a flag is not
/// evidence about the document, and `expected.json` states what the document
/// means — so anything needing `--min-overlap` or an explicit `--geometry` to
/// come out right is a bug here, not a configuration.
///
/// `document_name` is left unset: it only feeds each record's `link`, which no
/// expectation asserts.
fn extracted(pdf: &Path) -> Report {
    let bytes = std::fs::read(pdf).unwrap_or_else(|e| panic!("cannot read {}: {e}", pdf.display()));
    pdf_annotation_extractor::extract(bytes, &Options::default())
        .unwrap_or_else(|e| panic!("{}", pdf_annotation_extractor::error::report(&e).trim_end()))
}

fn records(pdf: &Path) -> Vec<Record> {
    extracted(pdf).annotations
}

/// Every sentinel appearing in a string, e.g. "E1A" from "... [[E1A]]".
fn sentinels(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 < chars.len() {
        if chars[i] == '[' && chars[i + 1] == '[' {
            let limit = chars.len().min(i + 14);
            if let Some(end) = (i + 2..limit)
                .find(|&j| j + 1 < chars.len() && chars[j] == ']' && chars[j + 1] == ']')
            {
                out.push(chars[i + 2..end].iter().collect());
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Our `kind` string for a PDF subtype name.
fn kind_for(subtype: &str) -> &'static str {
    match subtype {
        "Highlight" => "highlight",
        "Underline" => "underline",
        "StrikeOut" => "strikeout",
        "Squiggly" => "squiggly",
        "Text" => "note",
        "FreeText" => "freetext",
        "Ink" => "ink",
        "Square" => "square",
        // Deliberately a panic and not a fallback: a silent default would let
        // a typo through as a subtype that never matches anything.
        other => panic!("expected.json has an unmapped subtype: {other}"),
    }
}

/// Is this item paired by its sentinel?
///
/// Yes when its `covered_text` carries its own id. Some items cannot: two
/// annotations over identical words would have to share a sentinel, and
/// `pair` binds the expectation whose *id equals a sentinel in the extracted
/// text*, so one sentinel cannot serve two ids. Those are paired on page,
/// subtype and comment instead — and their `covered_text` is still compared
/// exactly by `text_matches`, which is the point of doing it this way rather
/// than keying on whether `covered_text` is set at all.
fn matched_by_sentinel(e: &ExpectedAnnot) -> bool {
    e.covered_text
        .as_deref()
        .is_some_and(|t| sentinels(t).contains(&e.id))
}

/// Pair each expectation with the record that claims it.
///
/// By sentinel where the expectation has one, by page + subtype + comment
/// where it does not. `validate` guarantees that every item has one or the
/// other, and that no two comment-matched items collide.
fn pair<'a>(exp: &'a [ExpectedAnnot], got: &'a [Record]) -> BTreeMap<String, Option<&'a Record>> {
    let mut by_id = BTreeMap::new();
    for e in exp {
        let found = if matched_by_sentinel(e) {
            got.iter().find(|r| {
                r.covered_text
                    .as_deref()
                    .is_some_and(|t| sentinels(t).contains(&e.id))
            })
        } else {
            got.iter().find(|r| {
                r.page == e.page
                    && r.kind == kind_for(&e.subtype)
                    && r.comment.as_deref() == e.comment.as_deref()
            })
        };
        by_id.insert(e.id.clone(), found);
    }
    by_id
}

/// Reject an `expected.json` that cannot be paired unambiguously.
///
/// Which branch `pair` takes is inferred rather than declared, which keeps the
/// schema small but makes a mistyped sentinel silent: the item would quietly
/// fall back to comment matching and fail with a message about a comment
/// nobody was thinking about. These four checks turn each such mistake into
/// a named error at load time.
fn validate(expected: &Expected, path: &Path) {
    for e in &expected.annotations {
        // A sentinel that is not this item's own id: a typo, or an id that was
        // renamed and missed in one place.
        if let Some(text) = e.covered_text.as_deref() {
            let found = sentinels(text);
            assert!(
                found.is_empty() || found.contains(&e.id),
                "{}: {} has covered_text carrying sentinels {:?} but not its \
                 own id — a typo, or a renamed id",
                path.display(),
                e.id,
                found
            );
        }

        // Nothing to pair on at all. Without a comment this would bind to
        // whichever record on the page happens to have a null comment.
        assert!(
            matched_by_sentinel(e) || e.comment.is_some(),
            "{}: {} has neither a sentinel nor a comment to pair on",
            path.display(),
            e.id
        );
    }

    // `find` returns the FIRST match, so two comment-matched items sharing
    // page, subtype and comment would both bind to one record and the second
    // would silently assert the first one's content.
    let mut seen: BTreeMap<(usize, &str, Option<&str>), &str> = BTreeMap::new();
    for e in &expected.annotations {
        if matched_by_sentinel(e) {
            continue;
        }
        let key = (e.page, e.subtype.as_str(), e.comment.as_deref());
        if let Some(prev) = seen.insert(key, e.id.as_str()) {
            panic!(
                "{}: {} and {} are both paired by comment and share page, \
                 subtype and comment — they would bind to the same record",
                path.display(),
                prev,
                e.id
            );
        }
    }

    // Ids must be unique, or `pair`'s map silently keeps one of them.
    let mut ids: Vec<&str> = expected.annotations.iter().map(|a| a.id.as_str()).collect();
    ids.sort_unstable();
    for w in ids.windows(2) {
        assert!(
            w[0] != w[1],
            "{}: duplicate item id {}",
            path.display(),
            w[0]
        );
    }
}

/// Non-whitespace characters, sorted: the comparison used when an item's
/// order is undefined.
fn multiset(s: &str) -> Vec<char> {
    let mut v: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    v.sort_unstable();
    v
}

/// What the extracted text is missing, and what it has in excess.
fn multiset_diff(got: &str, want: &str) -> (String, String) {
    let mut remaining = multiset(got);
    let mut missing: Vec<char> = Vec::new();
    for c in multiset(want) {
        match remaining.iter().position(|x| *x == c) {
            Some(pos) => {
                remaining.remove(pos);
            }
            None => missing.push(c),
        }
    }
    (
        missing.into_iter().collect(),
        remaining.into_iter().collect(),
    )
}

/// Does the extracted text satisfy this expectation?
///
/// Exact string comparison by default. For an item whose order is undefined
/// the boundaries are still fully asserted — nothing missing, nothing extra —
/// but the sequence is not.
fn text_matches(e: &ExpectedAnnot, got: Option<&str>) -> bool {
    match (got, e.covered_text.as_deref()) {
        (None, None) => true,
        (Some(g), Some(w)) if e.order.as_deref() == Some("undefined") => multiset(g) == multiset(w),
        (Some(g), Some(w)) => g == w,
        _ => false,
    }
}

/// The skip entries that apply to this producer file, keyed by item id.
fn skips_for<'a>(family: &'a Family, pdf: &Path) -> BTreeMap<&'a str, &'a str> {
    let name = pdf
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    family
        .skips
        .producers
        .get(&name)
        .map(|m| {
            m.iter()
                .map(|(id, e)| (id.as_str(), e.reason.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

fn label(family: &str, pdf: &Path) -> String {
    format!(
        "{family}/{}",
        pdf.file_name().unwrap_or_default().to_string_lossy()
    )
}

// ----------------------------------------------------------------------- tests

#[test]
fn producers_match_expected() {
    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for family in families() {
        for pdf in &family.producers {
            let got = records(pdf);
            let paired = pair(&family.expected.annotations, &got);
            let tag = label(&family.name, pdf);
            let skipped = skips_for(&family, pdf);

            for (id, reason) in &skipped {
                println!("[{tag}] {id}: not recorded by this producer — {reason}");
            }

            for e in &family.expected.annotations {
                if skipped.contains_key(e.id.as_str()) {
                    continue;
                }
                checked += 1;

                let Some(r) = paired[&e.id] else {
                    // Say which way the item was being paired. Reporting a
                    // missing sentinel for a comment-matched item names
                    // something that was never expected to exist.
                    failures.push(if matched_by_sentinel(e) {
                        // Look for a record with the same comment so the
                        // message shows what was extracted instead of leaving
                        // the reader to go digging.
                        let by_comment = got.iter().find(|r| {
                            r.page == e.page && r.comment.as_deref() == e.comment.as_deref()
                        });
                        match by_comment {
                            Some(r) => format!(
                                "[{tag}] {}: no record carries this sentinel; the record \
                                 with a matching comment has covered_text {:?}",
                                e.id, r.covered_text
                            ),
                            None => format!(
                                "[{tag}] {}: no record claims this item, and none carries \
                                 its comment either",
                                e.id
                            ),
                        }
                    } else {
                        format!(
                            "[{tag}] {}: paired by comment, but no {:?} record on page {} \
                             has comment {:?}",
                            e.id,
                            kind_for(&e.subtype),
                            e.page,
                            e.comment
                        )
                    });
                    continue;
                };

                if r.page != e.page {
                    failures.push(format!(
                        "[{tag}] {}: page {}, expected {}",
                        e.id, r.page, e.page
                    ));
                }

                let want_kind = kind_for(&e.subtype);
                if r.kind != want_kind {
                    failures.push(format!(
                        "[{tag}] {}: kind {:?}, expected {:?}",
                        e.id, r.kind, want_kind
                    ));
                }

                if !text_matches(e, r.covered_text.as_deref()) {
                    let mut msg = format!(
                        "[{tag}] {}: covered_text mismatch{}\n       got: {:?}\n      want: {:?}",
                        e.id,
                        if e.order.as_deref() == Some("undefined") {
                            " (order undefined; compared as a character multiset)"
                        } else {
                            ""
                        },
                        r.covered_text,
                        e.covered_text
                    );
                    if let (Some(g), Some(w)) =
                        (r.covered_text.as_deref(), e.covered_text.as_deref())
                    {
                        let (missing, extra) = multiset_diff(g, w);
                        if !missing.is_empty() || !extra.is_empty() {
                            msg.push_str(&format!(
                                "\n   missing: {missing:?}\n     extra: {extra:?}"
                            ));
                        }
                    }
                    failures.push(msg);
                }

                if r.comment.as_deref() != e.comment.as_deref() {
                    failures.push(format!(
                        "[{tag}] {}: comment mismatch\n       got: {:?}\n      want: {:?}",
                        e.id, r.comment, e.comment
                    ));
                }

                // null in expected.json means "any value": producers commonly
                // substitute the system user name.
                if e.author.is_some() && r.author.as_deref() != e.author.as_deref() {
                    failures.push(format!(
                        "[{tag}] {}: author {:?}, expected {:?}",
                        e.id, r.author, e.author
                    ));
                }

                // A comment-matched item is bound with `find`, which takes
                // the first hit, so a doubly-emitted quad-less annotation
                // would pair cleanly and go unnoticed. Sentinel-matched items
                // are covered by the duplicate check in
                // `no_duplicate_or_extra_records`; these are not.
                if !matched_by_sentinel(e) {
                    let n = got
                        .iter()
                        .filter(|r| {
                            r.page == e.page
                                && r.kind == kind_for(&e.subtype)
                                && r.comment.as_deref() == e.comment.as_deref()
                        })
                        .count();
                    if n > 1 {
                        failures.push(format!(
                            "[{tag}] {}: {n} records share page {}, kind {:?} and this \
                             item's comment — it is paired by comment, so they are \
                             indistinguishable",
                            e.id,
                            e.page,
                            kind_for(&e.subtype)
                        ));
                    }
                }

                // Bleed: text must carry its own sentinel and no other.
                if let Some(text) = r.covered_text.as_deref() {
                    let strays: Vec<String> =
                        sentinels(text).into_iter().filter(|s| *s != e.id).collect();
                    if !strays.is_empty() {
                        failures.push(format!(
                            "[{tag}] {}: foreign sentinels {:?} — the covered region \
                             is wrong\n       got: {:?}",
                            e.id, strays, text
                        ));
                    }
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} failure(s) across {checked} checked items:\n\n  {}\n\n\
         Every item is checked on every run; nothing is allowlisted. If a \
         failure is a known, open defect, it belongs in the escalation log in \
         that family's PRODUCERS.md — not suppressed here.\n",
        failures.len(),
        failures.join("\n  ")
    );
}

#[test]
fn unannotated_pages_produce_nothing() {
    for family in families() {
        for pdf in &family.producers {
            let got = records(pdf);
            let tag = label(&family.name, pdf);
            for page in &family.expected.pages_without_annotations {
                let n = got.iter().filter(|r| r.page == *page).count();
                assert_eq!(
                    n, 0,
                    "[{tag}] page {page} carries no annotations but {n} record(s) \
                     were emitted"
                );
            }
        }
    }
}

#[test]
fn no_duplicate_or_extra_records() {
    for family in families() {
        for pdf in &family.producers {
            let got = records(pdf);
            let tag = label(&family.name, pdf);

            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for r in &got {
                if let Some(t) = r.covered_text.as_deref() {
                    for s in sentinels(t) {
                        *counts.entry(s).or_default() += 1;
                    }
                }
            }
            let dupes: Vec<String> = counts
                .iter()
                .filter(|(_, n)| **n > 1)
                .map(|(s, n)| format!("{s} claimed by {n} records"))
                .collect();
            assert!(dupes.is_empty(), "[{tag}] {}", dupes.join("; "));

            // Every record should be attributable to some expected item, by
            // sentinel or by comment. A raw count would break as soon as one
            // producer fails to record an item, which is a fact about that
            // file rather than something worth asserting here.
            let known: Vec<String> = family
                .expected
                .annotations
                .iter()
                .map(|a| a.id.clone())
                .collect();
            let unattributed: Vec<String> = got
                .iter()
                .filter(|r| {
                    let by_sentinel = r
                        .covered_text
                        .as_deref()
                        .is_some_and(|t| sentinels(t).iter().any(|s| known.contains(s)));
                    let by_comment = family
                        .expected
                        .annotations
                        .iter()
                        .any(|a| a.comment.is_some() && a.comment == r.comment);
                    !by_sentinel && !by_comment
                })
                .map(|r| format!("page {} {:?}", r.page, r.covered_text))
                .collect();
            assert!(
                unattributed.is_empty(),
                "[{tag}] {} record(s) match no expected item:\n  {}",
                unattributed.len(),
                unattributed.join("\n  ")
            );
        }
    }
}

// --------------------------------------------------------------- diagnostics

/// No fixture may report a warning.
///
/// The strongest test in the suite, because a warning is how a plausible wrong
/// answer announces itself. Every other test asks whether the extracted text
/// matches; this one asks whether anything had to be assumed, skipped or
/// withheld to get it — and on a corpus of documents whose meaning is written
/// down, the answer should be no.
///
/// It ran clean on the whole corpus the day it was written: nine producers,
/// nothing reported. So this is not a target to work towards, it is a line
/// that already holds and should stay held.
///
/// Warnings only. `EncodingSniffed` and `UndefinedPdfDocBytes` are `Note`,
/// meaning something was assumed and the assumption is probably right; they
/// are printed when a run fails but do not fail it themselves.
#[test]
fn no_fixture_reports_a_warning() {
    let mut failures: Vec<String> = Vec::new();

    for family in families() {
        for pdf in &family.producers {
            let out = extracted(pdf);
            if out.diagnostics.warnings() == 0 {
                continue;
            }
            let tag = label(&family.name, pdf);
            let mut lines = vec![format!(
                "[{tag}] {} warning(s):",
                out.diagnostics.warnings()
            )];
            lines.extend(out.diagnostics.render().lines().map(|l| format!("    {l}")));

            // The per-annotation copies say which item to distrust, which the
            // document-level render cannot: it names a page, not an entry.
            for r in &out.annotations {
                for d in &r.diagnostics {
                    lines.push(format!("    -> page {} ({}): {d}", r.page, r.kind));
                }
            }
            failures.push(lines.join("\n"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} fixture(s) reported a warning:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Prove a diagnostic can reach a test at all.
///
/// The control for the test above, and the reason it is worth anything. An
/// assertion that nothing was reported passes just as happily when nothing
/// *can* be reported — a collector that never gets pushed to, a `Report` field
/// that never gets filled, a `render` that returns early — and the suite would
/// stay green through all three. `page_counts` is the one diagnostic that can
/// be switched on deliberately, so switching it on and finding it is the check.
#[test]
#[allow(clippy::field_reassign_with_default)]
fn the_diagnostic_channel_is_wired() {
    let family = families()
        .into_iter()
        .next()
        .expect("families() asserts it found some");
    let pdf = family
        .producers
        .first()
        .unwrap_or_else(|| panic!("{} has no producers", family.name))
        .clone();
    let bytes =
        std::fs::read(&pdf).unwrap_or_else(|e| panic!("cannot read {}: {e}", pdf.display()));

    // `Options` is `#[non_exhaustive]`, so no struct literal from out here.
    let mut opts = Options::default();
    opts.page_counts = true;
    let with = pdf_annotation_extractor::extract(bytes.clone(), &opts)
        .unwrap_or_else(|e| panic!("{}", pdf_annotation_extractor::error::report(&e).trim_end()));

    assert!(
        with.diagnostics.render().contains("in /Annots"),
        "asked for per-page counts and got nothing back, so a clean run in \
         `no_fixture_reports_a_warning` proves nothing.\nrendered: {:?}",
        with.diagnostics.render()
    );
    assert_eq!(
        with.diagnostics.warnings(),
        0,
        "a per-page count answers a question rather than raising a complaint, \
         so it must not inflate the count that --strict keys on"
    );

    // The same document reports nothing without the flag, so what arrived
    // above came from the flag and not from the document.
    let without = pdf_annotation_extractor::extract(bytes, &Options::default())
        .unwrap_or_else(|e| panic!("{}", pdf_annotation_extractor::error::report(&e).trim_end()));
    assert_eq!(without.diagnostics.render(), "");
}
