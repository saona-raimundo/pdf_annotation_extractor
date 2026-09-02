//! Fixture tests: run the binary over real annotated PDFs and compare against
//! each fixture family's `expected.json`.
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

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
    /// PDF subtype name: Highlight, Underline, StrikeOut, Squiggly, Text
    subtype: String,
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

// ------------------------------------------------------------- our own output

#[derive(Deserialize)]
struct Record {
    page: usize,
    kind: String,
    #[serde(default)]
    covered_text: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    author: Option<String>,
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

fn extract(pdf: &Path) -> Vec<Record> {
    let out = Command::new(env!("CARGO_BIN_EXE_pdf_annotation_extractor"))
        .arg(pdf)
        .arg("--format")
        .arg("json")
        .output()
        .unwrap_or_else(|e| panic!("failed to run the binary: {e}"));

    assert!(
        out.status.success(),
        "extractor exited with {} on {}\nstderr:\n{}",
        out.status,
        pdf.display(),
        String::from_utf8_lossy(&out.stderr)
    );

    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "output was not valid JSON for {}: {e}\nstdout:\n{}",
            pdf.display(),
            String::from_utf8_lossy(&out.stdout)
        )
    })
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
        other => panic!("expected.json has an unmapped subtype: {other}"),
    }
}

/// Pair each expectation with the record that claims it.
///
/// Sentinel matching wherever there is covered text; items without it (sticky
/// notes) fall back to page + subtype + comment.
fn pair<'a>(exp: &'a [ExpectedAnnot], got: &'a [Record]) -> BTreeMap<String, Option<&'a Record>> {
    let mut by_id = BTreeMap::new();
    for e in exp {
        let found = match &e.covered_text {
            Some(_) => got.iter().find(|r| {
                r.covered_text
                    .as_deref()
                    .is_some_and(|t| sentinels(t).contains(&e.id))
            }),
            None => got.iter().find(|r| {
                r.page == e.page
                    && r.kind == kind_for(&e.subtype)
                    && r.comment.as_deref() == e.comment.as_deref()
            }),
        };
        by_id.insert(e.id.clone(), found);
    }
    by_id
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
            let got = extract(pdf);
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
                    // Nothing carries the sentinel. Look for a record with the
                    // same comment so the message shows what was extracted
                    // instead of leaving the reader to go digging.
                    let by_comment = got
                        .iter()
                        .find(|r| r.page == e.page && r.comment.as_deref() == e.comment.as_deref());
                    failures.push(match by_comment {
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
            let got = extract(pdf);
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
            let got = extract(pdf);
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
