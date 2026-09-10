# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Also, we try to adhere to the [Conventional Commits specification](https://www.conventionalcommits.org/en/v1.0.0/).

## Unreleased

## [0.2.0]

- A library API. `extract(bytes, &Options) -> Result<Report, Error>`
- Structured diagnostics.
- `--strict`, which exits if anything was assumed or skipped.
- PDFDocEncoding.
- `/ActualText` support.
- Ligature folding for U+FB00–U+FB06, on by default, with `--keep-ligatures` to leave the presentation forms as the font mapped them.
- A `cli` feature, on by default. Library consumers can take `default-features = false` and skip clap entirely.
- Errors are a typed enum rather than `Box<dyn Error>`.
- Glyph geometry is inferred from the richest page sampled.
- `pdf_oxide` is pinned exactly.
- Glyphs are moved to the media box corner, so a page whose `/MediaBox` origin is not `(0, 0)` no longer loses every annotation on it.
- `date::human` had a branch that claimed to convert to the reader's timezone and did nothing.
- The declared licence is now a valid SPDX expression.

## [0.1.0] - 2026-09-02

- First working version
