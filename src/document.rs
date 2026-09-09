//! Reading facts out of the document: annotation dictionaries, strings,
//! colours, fonts and the outline.
//!
//! The strings are the delicate part. pdf_oxide decodes `/Contents` and `/T`
//! with `from_utf8_lossy`, which mangles the UTF-16BE most annotators write,
//! so these read the raw dictionary and decode it properly — and record which
//! encoding they had to assume.

use std::collections::HashMap;

use pdf_oxide::PdfDocument;
use pdf_oxide::annotation_types::AnnotationSubtype;
use pdf_oxide::annotations::Annotation;
use pdf_oxide::object::Object;
use pdf_oxide::outline::Destination;
use pdf_oxide::outline::OutlineItem;

use crate::actual_text;
use crate::pdf_string;
use crate::pdf_string::Encoding;

/// Count /Annots entries on a page and how many are inline dictionaries.
///
/// `get_annotations` skips any entry that is not an indirect reference, so this
/// is how annotations going missing becomes visible instead of guesswork.
pub(crate) fn audit_annots(doc: &PdfDocument, page: usize) -> (usize, usize) {
    let Ok(page_obj) = doc.get_page(page) else {
        return (0, 0);
    };
    let Some(dict) = page_obj.as_dict() else {
        return (0, 0);
    };
    let Some(raw) = dict.get("Annots") else {
        return (0, 0);
    };
    let resolved = doc.resolve_references(raw, 8).unwrap_or(Object::Null);
    let Some(arr) = resolved.as_array() else {
        return (0, 0);
    };
    let inline = arr
        .iter()
        .filter(|o| matches!(o, Object::Dictionary(_)))
        .count();
    (arr.len(), inline)
}

/// A text string read from the raw annotation dictionary.
///
/// The three states have to be distinguished. Collapsing `Empty` into
/// `Absent` and falling back to pdf_oxide's own field is what turned Okular's
/// empty comments into `??`: Okular writes a bare UTF-16BE BOM (`FE FF`, two
/// bytes) for an empty comment, which decodes correctly to nothing here, but
/// pdf_oxide's `from_utf8_lossy` reading of the same bytes is two replacement
/// characters. Falling back resurrected precisely the garbage this exists to
/// avoid.
pub(crate) enum RawString {
    /// Key not in the dictionary: use pdf_oxide's decoded field instead.
    Absent,
    /// Key present and decodes to nothing. A deliberate empty value, not a
    /// missing one, so do not fall back.
    Empty,
    Text(String),
}

/// What had to be assumed while decoding this document's strings.
///
/// Counted rather than warned about at the point of decoding: one line per
/// document is a diagnostic, one line per annotation is noise. Folds into the
/// structured `Report` when that lands.
#[derive(Default)]
pub(crate) struct EncodingNotes {
    /// Strings with no byte order mark, read as UTF-8 rather than as the
    /// PDFDocEncoding the spec prescribes.
    pub(crate) sniffed_utf8: usize,
    /// Bytes on PDFDocEncoding slots ISO 32000-1 Table D.2 leaves undefined.
    pub(crate) undefined_bytes: usize,
}

/// Decode a text string straight from the raw annotation dictionary.
pub(crate) fn raw_text(
    doc: &PdfDocument,
    a: &Annotation,
    key: &str,
    notes: &mut EncodingNotes,
) -> RawString {
    let Some(dict): Option<&HashMap<String, Object>> = a.raw_dict.as_ref() else {
        return RawString::Absent;
    };
    let Some(raw) = dict.get(key) else {
        return RawString::Absent;
    };
    let Ok(resolved) = doc.resolve_references(raw, 8) else {
        return RawString::Absent;
    };
    let Some(bytes) = resolved.as_string() else {
        return RawString::Absent;
    };
    let decoded = pdf_string::decode(bytes);
    if decoded.encoding == Encoding::Utf8Sniffed {
        notes.sniffed_utf8 += 1;
    }
    notes.undefined_bytes += decoded.undefined;

    let s = decoded.text.trim().to_string();
    if s.is_empty() {
        RawString::Empty
    } else {
        RawString::Text(s)
    }
}

/// Resolve a raw dictionary reading against pdf_oxide's own decoded field.
pub(crate) fn choose_text(raw: RawString, fallback: Option<&str>) -> Option<String> {
    match raw {
        RawString::Text(s) => Some(s),
        RawString::Empty => None,
        RawString::Absent => fallback
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    }
}

/// The text of a decoded PDF string, discarding what had to be assumed to get
/// it. See [`pdf_string`] for the encodings and the table.
///
/// Only the tests reach for this: production code wants the whole `Decoded`,
/// so that an assumption can be reported instead of silently made.
#[cfg(test)]
pub(crate) fn decode_pdf_string(bytes: &[u8]) -> String {
    pdf_string::decode(bytes).text
}

pub(crate) fn has_quads(a: &Annotation) -> bool {
    a.quad_points.as_ref().is_some_and(|q| !q.is_empty())
}

pub(crate) fn is_interesting(s: &AnnotationSubtype) -> bool {
    matches!(
        s,
        AnnotationSubtype::Highlight
            | AnnotationSubtype::Underline
            | AnnotationSubtype::StrikeOut
            | AnnotationSubtype::Squiggly
            | AnnotationSubtype::Text
            | AnnotationSubtype::FreeText
    )
}

pub(crate) fn subtype_name(s: &AnnotationSubtype) -> &'static str {
    match s {
        AnnotationSubtype::Highlight => "highlight",
        AnnotationSubtype::Underline => "underline",
        AnnotationSubtype::StrikeOut => "strikeout",
        AnnotationSubtype::Squiggly => "squiggly",
        AnnotationSubtype::Text => "note",
        AnnotationSubtype::FreeText => "freetext",
        _ => "other",
    }
}

/// `/Widths` for each simple font on a page, for the `/ActualText` scan.
///
/// Composite fonts use `/W` and a CID mapping instead; a span in one is
/// reported by `actual_text::scan` as covering no measurable text rather than
/// measured wrongly.
pub(crate) fn page_font_widths(
    doc: &PdfDocument,
    page: usize,
) -> HashMap<String, actual_text::FontWidths> {
    let mut out = HashMap::new();
    let number = |o: &Object| -> Option<f32> {
        o.as_real()
            .map(|v| v as f32)
            .or_else(|| o.as_integer().map(|v| v as f32))
    };
    let resolve = |o: &Object| doc.resolve_references(o, 8).ok();

    let Ok(page_obj) = doc.get_page(page) else {
        return out;
    };
    let fonts = page_obj
        .as_dict()
        .and_then(|d| d.get("Resources"))
        .and_then(&resolve)
        .and_then(|r| r.as_dict().and_then(|d| d.get("Font")).and_then(&resolve));
    let Some(fonts) = fonts.as_ref().and_then(|f| f.as_dict()) else {
        return out;
    };

    for (name, entry) in fonts {
        let Some(font) = resolve(entry) else { continue };
        let Some(dict) = font.as_dict() else { continue };
        let first_char = dict.get("FirstChar").and_then(&number).unwrap_or(0.0) as i64;
        let widths: Vec<f32> = dict
            .get("Widths")
            .and_then(&resolve)
            .and_then(|w| w.as_array().map(|a| a.iter().filter_map(&number).collect()))
            .unwrap_or_default();
        let missing = dict
            .get("FontDescriptor")
            .and_then(&resolve)
            .and_then(|d| {
                d.as_dict()
                    .and_then(|d| d.get("MissingWidth"))
                    .and_then(&number)
            })
            .unwrap_or(0.0);
        out.insert(
            name.clone(),
            actual_text::FontWidths {
                first_char,
                widths,
                missing,
            },
        );
    }
    out
}

pub(crate) fn section_titles(doc: &PdfDocument, page_count: usize) -> HashMap<usize, String> {
    let mut flat: Vec<(usize, String)> = Vec::new();
    if let Ok(Some(items)) = doc.get_outline() {
        walk(&items, &mut flat);
    }
    flat.sort_by_key(|(p, _)| *p);

    let mut map = HashMap::new();
    if flat.is_empty() {
        return map;
    }
    for page in 0..page_count {
        if let Some((_, title)) = flat.iter().rfind(|(p, _)| *p <= page) {
            map.insert(page, title.clone());
        }
    }
    map
}

pub(crate) fn walk(items: &[OutlineItem], out: &mut Vec<(usize, String)>) {
    for item in items {
        if let Some(Destination::PageIndex(p)) = item.dest {
            out.push((p, item.title.clone()));
        }
        walk(&item.children, out);
    }
}

pub(crate) fn hex_color(c: &[f64]) -> Option<String> {
    let to8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    match c {
        [r, g, b] => Some(format!("#{:02x}{:02x}{:02x}", to8(*r), to8(*g), to8(*b))),
        [g] => {
            let v = to8(*g);
            Some(format!("#{v:02x}{v:02x}{v:02x}"))
        }
        [c_, m, y, k] => {
            let f = |x: f64| to8((1.0 - x) * (1.0 - k));
            Some(format!("#{:02x}{:02x}{:02x}", f(*c_), f(*m), f(*y)))
        }
        _ => None,
    }
}
