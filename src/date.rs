//! PDF date strings.
//!
//! `/M` and `/CreationDate` use the syntax of ISO 32000-1 §7.9.4:
//!
//! ```text
//! D:YYYYMMDDHHmmSSOHH'mm'
//! ```
//!
//! Everything after the year is optional, the `D:` prefix is often but not
//! always present, and the offset is `Z`, `+HH'mm'` or `-HH'mm'` with the
//! trailing apostrophe optional. pdf_oxide hands the string over untouched,
//! so parsing it is on us. Papers and Okular both write the full form:
//! `D:20260902091617+01'00'`.
//!
//! When no offset is given the spec says the relationship to UT is *unknown* —
//! not UTC. We take it at face value and print the civil time as written,
//! rather than inventing an offset and shifting the clock.

use jiff::civil::{Date, DateTime};
use jiff::tz::{Offset, TimeZone};

/// A PDF date, formatted for a person to read: `2026-09-02 09:16`.
///
/// Returns `None` for anything unparseable, so a malformed date is simply
/// omitted from a report rather than failing the run.
pub(crate) fn human(pdf_date: &str) -> Option<String> {
    let (dt, offset) = parse(pdf_date)?;
    match offset {
        // Present the annotation time in reader's zone.
        Some(_off) => {
            let zoned = dt.to_zoned(TimeZone::system()).ok()?;
            Some(zoned.strftime("%Y-%m-%d %H:%M").to_string())
        }
        None => Some(dt.strftime("%Y-%m-%d %H:%M").to_string()),
    }
}

/// The same instant with its offset, for callers that need to compare or sort.
pub(crate) fn parse(pdf_date: &str) -> Option<(DateTime, Option<Offset>)> {
    let s = pdf_date.trim();
    let s = s.strip_prefix("D:").unwrap_or(s);

    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return None;
    }

    // Fields default per the spec: month and day to 01, times to 00.
    let field = |start: usize, len: usize, default: i64| -> Option<i64> {
        if digits.len() < start + len {
            return Some(default);
        }
        digits[start..start + len].parse::<i64>().ok()
    };

    let year = digits[0..4].parse::<i16>().ok()?;
    let month = field(4, 2, 1)? as i8;
    let day = field(6, 2, 1)? as i8;
    let hour = field(8, 2, 0)? as i8;
    let minute = field(10, 2, 0)? as i8;
    let second = field(12, 2, 0)? as i8;

    // Date::new rejects month 13 and 31 February, so a corrupt string cannot
    // produce a plausible-looking wrong date.
    let date = Date::new(year, month, day).ok()?;
    let dt = date.at(hour, minute, second, 0);

    let offset = parse_offset(&s[digits.len()..]);
    Some((dt, offset))
}

/// `Z`, `+HH'mm'`, `-HH'mm'`, and the variants that drop the apostrophes or
/// the minutes entirely. Anything else counts as no offset given.
fn parse_offset(rest: &str) -> Option<Offset> {
    let rest = rest.trim();
    let sign = match rest.chars().next()? {
        'Z' | 'z' => return Offset::from_seconds(0).ok(),
        '+' => 1,
        '-' => -1,
        _ => return None,
    };

    let body: String = rest[1..]
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '\'')
        .collect();
    let mut parts = body.split('\'').filter(|p| !p.is_empty());

    let hours: i32 = parts.next().unwrap_or("0").parse().ok()?;
    let minutes: i32 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    if hours > 23 || minutes > 59 {
        return None;
    }

    Offset::from_seconds(sign * (hours * 3600 + minutes * 60)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_what_papers_and_okular_write() {
        // The exact strings from the fixture PDFs.
        assert_eq!(
            human("D:20260902091617+01'00'").as_deref(),
            Some("2026-09-02 09:16")
        );
        assert_eq!(
            human("D:20260902104045+01'00'").as_deref(),
            Some("2026-09-02 10:40")
        );
    }

    #[test]
    fn accepts_the_optional_pieces() {
        // No D: prefix.
        assert_eq!(
            human("20260902091617Z").as_deref(),
            Some("2026-09-02 09:16")
        );
        // Date only: times default to zero.
        assert_eq!(human("D:20260902").as_deref(), Some("2026-09-02 00:00"));
        // Year only: month and day default to 01.
        assert_eq!(human("D:2026").as_deref(), Some("2026-01-01 00:00"));
        // Trailing apostrophe dropped, as PDF 2.0 permits.
        assert_eq!(
            human("D:20260902091617-05'30").as_deref(),
            Some("2026-09-02 09:16")
        );
        // Offset hours with no minutes.
        assert_eq!(
            human("D:20260902091617+02").as_deref(),
            Some("2026-09-02 09:16")
        );
    }

    #[test]
    fn keeps_the_recorded_offset_rather_than_shifting() {
        // Same civil time, three different offsets: the printed clock time is
        // the one the annotator saw, not a conversion into anyone else's zone.
        for off in ["+01'00'", "-08'00'", "Z"] {
            assert_eq!(
                human(&format!("D:20260902091617{off}")).as_deref(),
                Some("2026-09-02 09:16")
            );
        }
    }

    #[test]
    fn missing_offset_is_not_treated_as_utc() {
        // The spec says the relationship to UT is unknown, so the civil time
        // is printed as written rather than shifted by an invented offset.
        let (dt, offset) = parse("D:20260902091617").unwrap();
        assert!(offset.is_none());
        assert_eq!(
            dt.strftime("%Y-%m-%d %H:%M").to_string(),
            "2026-09-02 09:16"
        );
    }

    #[test]
    fn rejects_malformed_dates() {
        assert_eq!(human(""), None);
        assert_eq!(human("D:"), None);
        assert_eq!(human("not a date"), None);
        assert_eq!(human("D:202"), None); // short year
        assert_eq!(human("D:20261302091617Z"), None); // month 13
        assert_eq!(human("D:20260230091617Z"), None); // 30 February
    }

    #[test]
    fn ignores_a_nonsense_offset_rather_than_failing() {
        // A bad offset should not lose the date, which is the useful part.
        let (_, offset) = parse("D:20260902091617+99'99'").unwrap();
        assert!(offset.is_none());
        assert_eq!(
            human("D:20260902091617+99'99'").as_deref(),
            Some("2026-09-02 09:16")
        );
    }
}
