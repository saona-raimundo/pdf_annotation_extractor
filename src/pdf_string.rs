//! Decoding PDF text strings.
//!
//! ISO 32000-1 §7.9.2.2 gives a text string two forms: UTF-16BE preceded by a
//! byte order mark, or PDFDocEncoded. Producers add a third in practice, bare
//! UTF-8, and a fourth by accident, UTF-16LE with a reversed mark.
//!
//! PDFDocEncoding is not Latin-1. It agrees with it from U+00A1 upwards, but
//! 0x18–0x1F carry accents and 0x80–0xA0 carry the punctuation a word
//! processor emits without asking: curly quotes, en and em dashes, the
//! ellipsis, the bullet. Those are exactly the characters an academic comment
//! is made of, so reading the block as Latin-1 turns `don't` into `don\u{92}t`
//! and an em dash into a control character. Nothing in the fixture corpus
//! exercises this — every producer we have surveyed writes UTF-16BE with a
//! mark, or plain ASCII — which is why the table is carried by the tests
//! below rather than by a fixture.
//!
//! Table D.2 leaves 0x7F, 0x9F and 0xAD undefined. We decode them as Latin-1
//! and count them, because a producer writing 0xAD almost certainly means a
//! soft hyphen and the alternative is dropping a character silently.

/// What the bytes were taken to be.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    /// Byte order mark FE FF. The spec's own form.
    Utf16Be,
    /// Byte order mark FF FE. Off spec, but written.
    Utf16Le,
    /// Byte order mark EF BB BF. Off spec, and unambiguous.
    Utf8Bom,
    /// No mark, but valid UTF-8 with at least one multi-byte sequence.
    Utf8Sniffed,
    /// No mark and not obviously UTF-8: the spec's default.
    PdfDoc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Decoded {
    pub text: String,
    pub encoding: Encoding,
    /// Bytes that landed on a PDFDocEncoding slot the spec leaves undefined.
    pub undefined: usize,
}

/// Decode a PDF text string.
///
/// The order of the tests matters: a byte order mark is decisive, so those go
/// first, and the UTF-8 sniff runs before PDFDocEncoding only for strings that
/// carry a valid multi-byte sequence.
///
/// The sniff is a judgement call. The spec says a string without a mark *is*
/// PDFDocEncoded, and a PDFDocEncoded string can be valid UTF-8 by
/// coincidence: `C3 A9` reads as `Ã©` under the table and `é` under UTF-8. But
/// a producer that emits `C3 A9` for `é` is far more common than one that
/// means `Ã©`, and the outcome is reported either way, so the caller can say
/// which assumption it made rather than the reader having to guess from the
/// mojibake.
pub(crate) fn decode(bytes: &[u8]) -> Decoded {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return Decoded {
            text: from_utf16(rest, u16::from_be_bytes),
            encoding: Encoding::Utf16Be,
            undefined: 0,
        };
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return Decoded {
            text: from_utf16(rest, u16::from_le_bytes),
            encoding: Encoding::Utf16Le,
            undefined: 0,
        };
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return Decoded {
            text: String::from_utf8_lossy(rest).into_owned(),
            encoding: Encoding::Utf8Bom,
            undefined: 0,
        };
    }
    if bytes.iter().any(|&b| b >= 0x80)
        && let Ok(s) = std::str::from_utf8(bytes)
    {
        return Decoded {
            text: s.to_string(),
            encoding: Encoding::Utf8Sniffed,
            undefined: 0,
        };
    }

    let mut text = String::with_capacity(bytes.len());
    let mut undefined = 0;
    for &b in bytes {
        let (c, defined) = pdfdoc(b);
        if !defined {
            undefined += 1;
        }
        text.push(c);
    }
    Decoded {
        text,
        encoding: Encoding::PdfDoc,
        undefined,
    }
}

/// One PDFDocEncoded byte, and whether the spec defines that slot.
fn pdfdoc(b: u8) -> (char, bool) {
    match b {
        // 0x18–0x1F: accents, where ASCII has control characters.
        0x18..=0x1F => (ACCENTS[(b - 0x18) as usize], true),
        // 0x7F, 0x9F, 0xAD: undefined. Latin-1 by courtesy; see the module docs.
        0x7F | 0x9F | 0xAD => (b as char, false),
        // 0x80–0xA0: the punctuation block. Everything else is Latin-1, which
        // for a single byte is the identity onto U+0000–U+00FF.
        0x80..=0xA0 => (HIGH[(b - 0x80) as usize], true),
        _ => (b as char, true),
    }
}

/// 0x18–0x1F. ISO 32000-1, Table D.2.
static ACCENTS: [char; 8] = [
    '\u{02D8}', // breve
    '\u{02C7}', // caron
    '\u{02C6}', // modifier letter circumflex accent
    '\u{02D9}', // dot above
    '\u{02DD}', // double acute accent
    '\u{02DB}', // ogonek
    '\u{02DA}', // ring above
    '\u{02DC}', // small tilde
];

/// 0x80–0xA0. ISO 32000-1, Table D.2. 0x9F is undefined and never read from
/// here — `pdfdoc` intercepts it — but the slot is kept so the index is the
/// byte offset and the table can be checked against the standard line by line.
///
/// Note 0x93 and 0x94: a PDFDocEncoded *comment* can legitimately contain
/// U+FB01 and U+FB02. Ligature folding is a text-recovery transform and must
/// not touch comments, and this is the reason it cannot be applied blindly to
/// every string in the file.
static HIGH: [char; 33] = [
    '\u{2022}', // 0x80 bullet
    '\u{2020}', // 0x81 dagger
    '\u{2021}', // 0x82 double dagger
    '\u{2026}', // 0x83 horizontal ellipsis
    '\u{2014}', // 0x84 em dash
    '\u{2013}', // 0x85 en dash
    '\u{0192}', // 0x86 latin small letter f with hook
    '\u{2044}', // 0x87 fraction slash
    '\u{2039}', // 0x88 single left-pointing angle quotation mark
    '\u{203A}', // 0x89 single right-pointing angle quotation mark
    '\u{2212}', // 0x8A minus sign
    '\u{2030}', // 0x8B per mille sign
    '\u{201E}', // 0x8C double low-9 quotation mark
    '\u{201C}', // 0x8D left double quotation mark
    '\u{201D}', // 0x8E right double quotation mark
    '\u{2018}', // 0x8F left single quotation mark
    '\u{2019}', // 0x90 right single quotation mark
    '\u{201A}', // 0x91 single low-9 quotation mark
    '\u{2122}', // 0x92 trade mark sign
    '\u{FB01}', // 0x93 latin small ligature fi
    '\u{FB02}', // 0x94 latin small ligature fl
    '\u{0141}', // 0x95 latin capital letter l with stroke
    '\u{0152}', // 0x96 latin capital ligature oe
    '\u{0160}', // 0x97 latin capital letter s with caron
    '\u{0178}', // 0x98 latin capital letter y with diaeresis
    '\u{017D}', // 0x99 latin capital letter z with caron
    '\u{0131}', // 0x9A latin small letter dotless i
    '\u{0142}', // 0x9B latin small letter l with stroke
    '\u{0153}', // 0x9C latin small ligature oe
    '\u{0161}', // 0x9D latin small letter s with caron
    '\u{017E}', // 0x9E latin small letter z with caron
    '\u{FFFD}', // 0x9F undefined
    '\u{20AC}', // 0xA0 euro sign
];

/// UTF-16 code units, big or little endian, tolerating an odd trailing byte.
///
/// `as_chunks` drops a lone final byte rather than panicking: a truncated
/// string should lose its last character, not the run.
fn from_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().copied().map(unit).collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> String {
        decode(bytes).text
    }

    // ------------------------------------------------------ byte order marks

    #[test]
    fn utf16be_mark_wins() {
        let d = decode(b"\xFE\xFF\x00C\x00a\x00f\x00\xE9");
        assert_eq!(d.text, "Café");
        assert_eq!(d.encoding, Encoding::Utf16Be);
    }

    #[test]
    fn utf16le_mark_is_accepted_though_off_spec() {
        let d = decode(b"\xFF\xFEC\x00a\x00f\x00\xE9\x00");
        assert_eq!(d.text, "Café");
        assert_eq!(d.encoding, Encoding::Utf16Le);
    }

    #[test]
    fn utf8_mark_is_accepted_though_off_spec() {
        let d = decode(b"\xEF\xBB\xBFCaf\xC3\xA9");
        assert_eq!(d.text, "Café");
        assert_eq!(d.encoding, Encoding::Utf8Bom);
    }

    #[test]
    fn a_bare_mark_decodes_to_nothing() {
        // The two bytes Okular writes for an empty comment.
        assert_eq!(text(b"\xFE\xFF"), "");
        assert_eq!(text(b""), "");
    }

    #[test]
    fn an_odd_trailing_byte_loses_a_character_not_the_string() {
        assert_eq!(text(b"\xFE\xFF\x00T\x00"), "T");
    }

    #[test]
    fn decodes_a_surrogate_pair() {
        assert_eq!(text(b"\xFE\xFF\xD8=\xDE\x00"), "\u{1F600}");
    }

    // ------------------------------------------------------ PDFDocEncoding
    // Pins: the block the whole table exists for. Read as Latin-1 these come
    // out as control characters, so a comment with a curly apostrophe or an em
    // dash — which is most comments a word processor touched — is corrupted in
    // a way that survives into the report.

    #[test]
    fn ascii_is_unchanged_and_reported_as_pdfdoc() {
        let d = decode(b"plain comment");
        assert_eq!(d.text, "plain comment");
        assert_eq!(d.encoding, Encoding::PdfDoc);
        assert_eq!(d.undefined, 0);
    }

    #[test]
    fn decodes_the_punctuation_block() {
        // 0x90 is quoteright, not 0x92 — the table is ordered by glyph name,
        // so the apostrophe and the trade mark sit two apart and swapping
        // them is the easy mistake.
        assert_eq!(text(b"don\x90t"), "don\u{2019}t");
        assert_eq!(text(b"\x8Dquoted\x8E"), "\u{201C}quoted\u{201D}");
        assert_eq!(text(b"a \x84 dash"), "a — dash");
        assert_eq!(text(b"an \x85 dash"), "an – dash");
        assert_eq!(text(b"\x80 item"), "• item");
        assert_eq!(text(b"and so on\x83"), "and so on…");
        assert_eq!(text(b"\x8Flow\x91"), "\u{2018}low\u{201A}");
    }

    #[test]
    fn decodes_the_ligature_and_symbol_slots() {
        // 0x93/0x94 put U+FB01 and U+FB02 in a comment legitimately.
        assert_eq!(text(b"\x93nal"), "\u{FB01}nal");
        assert_eq!(text(b"\x94ow"), "\u{FB02}ow");
        assert_eq!(text(b"Widget\x92"), "Widget™");
        assert_eq!(text(b"\xA0100"), "€100");
        assert_eq!(text(b"\x8A5"), "\u{2212}5");
        assert_eq!(text(b"\x8B"), "\u{2030}");
        assert_eq!(text(b"\x9Ccuvre"), "\u{0153}cuvre");
    }

    #[test]
    fn decodes_the_accent_block_where_ascii_has_controls() {
        assert_eq!(text(b"\x18\x19\x1A"), "\u{02D8}\u{02C7}\u{02C6}");
        assert_eq!(text(b"\x1F"), "\u{02DC}");
    }

    #[test]
    fn tab_newline_and_return_survive() {
        // Below 0x18, so the accent table must not reach them.
        assert_eq!(text(b"a\tb\nc\rd"), "a\tb\nc\rd");
    }

    #[test]
    fn agrees_with_latin1_from_a1_upwards() {
        assert_eq!(text(b"caf\xE9"), "café");
        assert_eq!(text(b"\xDCber"), "Über");
        assert_eq!(text(b"\xA1"), "¡");
    }

    #[test]
    fn undefined_slots_are_counted_not_dropped() {
        let d = decode(b"soft\xADhyphen");
        assert_eq!(d.text, "soft\u{00AD}hyphen");
        assert_eq!(d.undefined, 1);
        assert_eq!(decode(b"\x7F\x9F\xAD").undefined, 3);
    }

    // ------------------------------------------------------ the UTF-8 sniff

    #[test]
    fn bare_utf8_is_sniffed_when_multi_byte() {
        let d = decode(b"caf\xC3\xA9");
        assert_eq!(d.text, "café");
        assert_eq!(d.encoding, Encoding::Utf8Sniffed);
    }

    #[test]
    fn high_bytes_that_are_not_utf8_fall_to_the_table() {
        // 0x90 alone is a continuation byte with nothing to continue, so it is
        // not valid UTF-8 and there is no ambiguity to resolve.
        let d = decode(b"don\x90t");
        assert_eq!(d.text, "don\u{2019}t");
        assert_eq!(d.encoding, Encoding::PdfDoc);
    }

    #[test]
    fn the_sniff_does_not_claim_pure_ascii() {
        // Both readings agree on ASCII; the spec's default is the honest label.
        assert_eq!(decode(b"plain").encoding, Encoding::PdfDoc);
    }
}
