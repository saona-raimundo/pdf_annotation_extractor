# pdf_annotation_extractor

Extracts annotations (highlights, comments, notes) from a PDF, outputs Markdown or JSON. Intended for sharing feedback in plain text instead of the annotated PDF.

**Main problem**
Text markup in PDFs records *where* a highlight is, not the text underneath. It needs to be retrieved.

**Output example**
```
## Comments

- Page #4:
  > Quote text highlighted in the PDF.

    Your comment in the annotation.
```

## Installation

```
cargo install pdf_annotation_extractor
```

## Usage

```
pdf_annotation_extractor paper.pdf              # Markdown
pdf_annotation_extractor paper.pdf -f json      # JSON
```

## Output options

`--show colour,kind,date` adds identifiers beside the page number.
`--number global` or `--number per-page` numbers items, so a remark can be cited as "page 3, note 2".

Anything finer (bullet, indent, headings, escaping) is set through `markdown::Style`, which the library exposes. The CLI uses the defaults.

## Library

```
cargo add pdf_annotation_extractor --no-default-features
```

`default-features = false` drops `clap`, which only the binary uses.

## Similar tools

[pdfannots](https://github.com/0xabu/pdfannots) tackles the same problem.

[pdfannots2json](https://github.com/mgmeyers/pdfannots2json) tackles the same problem, focused on JSON output.

[pdf-annotations-converter](https://crates.io/crates/pdf-annotations-converter) focuses on GoodReader's text export.

[pdf-annot](https://crates.io/crates/pdf-annot) reads, creates, and edits PDF annotations. Part of [PDFluent](https://pdfluent.com/)

[cli-pdf-extract](https://crates.io/crates/cli-pdf-extract) focuses on LLM-friendly PDF inspection. 


## Tests

We test annotations inserted by hand using various PDF readers. Happy to include more if your case is not covered.

## Known issues and limitations

- Right-to-left scripts come out reversed.

Text is assembled by sorting glyphs left to right. 
Moreover, there is no continuation of reading order by looking just at an annotation. Therefore, this is unsupported.

- Markup without `/QuadPoints` yields the comment but no covered text. 

The PDF specification requires quads on text markup and no producer considered omits them.

- Reply threads (`/IRT`) are not followed, so a reply appears as a separate item. `/Caret` insertion marks are skipped.

There is no "reply" or "caret" options in many PDF readers, so this is not in scope for now.

- Line-break hyphens are joined, which suits LaTeX multi-column text ("soft-`\n`ware" to "software") but not always ("memory-`\n`mapped" to "memorymapped").

These cases cannot be told apart. `--keep-hyphens` disables the default behaviour.
