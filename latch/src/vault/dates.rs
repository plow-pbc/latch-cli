//! Dates as forms want them, ported from the Latch app with its tests. The
//! vault keeps one shape, `YYYY-MM-DD` for a date of birth and `YYYY-MM` for
//! a card's expiry; the page decides the other.
//!
//! Pattern tokens: `YYYY YY MMMM MMM MM M DD D Do`; anything that is not a
//! letter passes through, and a letter that is not a token is refused
//! rather than guessed. `DD`, `D` and `Do` need a day, which a month-only
//! date does not have.

/// The labels whose values are dates, with the shape used when the field
/// says nothing (empty: as stored).
pub const DATE_OF_BIRTH: &str = "date of birth";
pub const CARD_EXPIRY: &str = "expiry";

pub fn default_shape(label: &str) -> Option<&'static str> {
    match label {
        DATE_OF_BIRTH => Some(""),
        CARD_EXPIRY => Some("MM/YY"),
        _ => None,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const TOKENS: [&str; 9] = ["YYYY", "YY", "MMMM", "MMM", "MM", "M", "DD", "Do", "D"];

struct Date {
    y: u32,
    m: u32,
    d: Option<u32>,
}

fn days_in(y: u32, m: u32) -> u32 {
    match m {
        2 if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn parse_iso(iso: &str) -> Result<Date, String> {
    let bad = || "a date is stored as YYYY-MM-DD, or YYYY-MM for a month".to_string();
    let parts: Vec<&str> = iso.split('-').collect();
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let (y, m, d) = match parts.as_slice() {
        [y, m] if digits(y, 4) && digits(m, 2) => (y, m, None),
        [y, m, d] if digits(y, 4) && digits(m, 2) && digits(d, 2) => (y, m, Some(d)),
        _ => return Err(bad()),
    };
    let (y, m) = (y.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if !(1..=12).contains(&m) {
        return Err(bad());
    }
    let d = match d {
        Some(d) => {
            let d: u32 = d.parse().map_err(|_| bad())?;
            if d == 0 || d > days_in(y, m) {
                return Err(bad());
            }
            Some(d)
        }
        None => None,
    };
    Ok(Date { y, m, d })
}

fn ordinal(day: u32) -> String {
    let suffix = match (day % 100, day % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

/// `iso` rendered with `pattern`.
pub fn format(iso: &str, pattern: &str) -> Result<String, String> {
    let date = parse_iso(iso)?;
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(first) = rest.chars().next() {
        if !first.is_ascii_alphabetic() {
            let end = rest
                .find(|c: char| c.is_ascii_alphabetic())
                .unwrap_or(rest.len());
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        let Some(token) = TOKENS.iter().find(|t| rest.starts_with(**t)) else {
            return Err(format!(
                "{first:?} is not a date token; the tokens are YYYY, YY, MMMM (November), \
                 MMM (Nov), MM (11), M (11 or 5), DD (09), D (9) and Do (9th), and anything \
                 else is typed as written"
            ));
        };
        let day = || {
            date.d.ok_or_else(|| {
                format!("{token:?} needs a day, and this date has no day: it is a month and a year")
            })
        };
        let piece = match *token {
            "YYYY" => format!("{:04}", date.y),
            "YY" => format!("{:02}", date.y % 100),
            "MMMM" => MONTHS[date.m as usize - 1].to_string(),
            "MMM" => MONTHS[date.m as usize - 1][..3].to_string(),
            "MM" => format!("{:02}", date.m),
            "M" => date.m.to_string(),
            "DD" => format!("{:02}", day()?),
            "D" => day()?.to_string(),
            _ => ordinal(day()?),
        };
        out.push_str(&piece);
        rest = &rest[token.len()..];
    }
    Ok(out)
}

const EXPIRY_ERROR: &str = "a card expiry is a month 1-12 and a two- or four-digit year";

/// One stored part of a card's expiry, normalised (`04`, `2031`).
pub fn expiry_part(part: &str, given: &str) -> Result<String, String> {
    let trimmed = given.trim();
    let digits = !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit());
    if part == "expMonth" {
        if digits && trimmed.len() <= 2 {
            let month: u32 = trimmed.parse().map_err(|_| EXPIRY_ERROR.to_string())?;
            if (1..=12).contains(&month) {
                return Ok(format!("{month:02}"));
            }
        }
    } else if digits && trimmed.len() == 2 {
        return Ok(format!("20{trimmed}"));
    } else if digits && trimmed.len() == 4 {
        return Ok(trimmed.to_string());
    }
    Err(EXPIRY_ERROR.to_string())
}

/// A card's expiry as the month it is for (`2031-04`), if both parts read.
pub fn expiry_iso(month: Option<&str>, year: Option<&str>) -> Option<String> {
    let year = expiry_part("expYear", year?).ok()?;
    let month = expiry_part("expMonth", month?).ok()?;
    Some(format!("{year}-{month}"))
}

/// The pattern a page's field asks for, read from its placeholder
/// (`MM/YY`, `mm / yyyy`) or, failing that, from how many characters it
/// takes; `None` when it says nothing usable.
pub fn pattern_of(placeholder: &str, max_length: Option<u32>, label: &str) -> Option<String> {
    let upper = placeholder.trim().to_uppercase();
    if !upper.is_empty() && upper.chars().any(|c| c.is_ascii_alphabetic()) {
        // Accepted only if the date tokens alone make it up.
        let probe = if label == CARD_EXPIRY {
            "2000-01"
        } else {
            "2000-01-01"
        };
        let only_tokens = upper.replace("DO", "Do");
        if format(probe, &only_tokens).is_ok() {
            return Some(only_tokens);
        }
    }
    if label == CARD_EXPIRY {
        return match max_length {
            Some(4) => Some("MMYY".into()),
            Some(5) => Some("MM/YY".into()),
            Some(7) => Some("MM/YYYY".into()),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_render_a_full_date() {
        for (pattern, expected) in [
            ("YYYY-MM-DD", "1984-11-09"),
            ("MM/DD/YYYY", "11/09/1984"),
            ("M/D/YY", "11/9/84"),
            ("DD.MM.YYYY", "09.11.1984"),
            ("MMMM Do, YYYY", "November 9th, 1984"),
            ("MMM D YYYY", "Nov 9 1984"),
            ("MMMM", "November"),
            ("M", "11"),
            ("YYYY", "1984"),
            ("D", "9"),
        ] {
            assert_eq!(
                format("1984-11-09", pattern).unwrap(),
                expected,
                "{pattern}"
            );
        }
        for (iso, expected) in [
            ("2000-05-01", "1st"),
            ("2000-05-02", "2nd"),
            ("2000-05-03", "3rd"),
            ("2000-05-11", "11th"),
            ("2000-05-12", "12th"),
            ("2000-05-13", "13th"),
            ("2000-05-21", "21st"),
            ("2000-05-22", "22nd"),
            ("2000-05-23", "23rd"),
            ("2000-05-31", "31st"),
        ] {
            assert_eq!(format(iso, "Do").unwrap(), expected, "{iso}");
        }
        assert_eq!(format("2000-05-01", "M").unwrap(), "5");
        assert_eq!(format("0050-01-01", "YYYY-MM-DD").unwrap(), "0050-01-01");
        assert_eq!(
            format("0001-12-25", "MMMM Do, YYYY").unwrap(),
            "December 25th, 0001"
        );
    }

    #[test]
    fn bad_tokens_and_dates_are_refused() {
        assert!(
            format("1984-11-09", "MM/DD/YYYY at hh")
                .unwrap_err()
                .starts_with("'a'")
        );
        assert!(format("1984-11-09", "YYYYY").is_err());
        for iso in ["11/09/1984", "1984-13-09", "1984-02-30"] {
            assert!(
                format(iso, "YYYY").unwrap_err().contains("YYYY-MM-DD"),
                "{iso}"
            );
        }
    }

    #[test]
    fn a_month_only_date_has_no_day() {
        for (pattern, expected) in [
            ("MM/YY", "04/31"),
            ("MM/YYYY", "04/2031"),
            ("MMMM YYYY", "April 2031"),
            ("M", "4"),
            ("YYYY-MM", "2031-04"),
        ] {
            assert_eq!(format("2031-04", pattern).unwrap(), expected);
        }
        for pattern in ["DD", "MM/DD/YYYY", "Do"] {
            assert!(
                format("2031-04", pattern).unwrap_err().contains("no day"),
                "{pattern}"
            );
        }
        for iso in ["2031-4", "2031-13"] {
            assert!(format(iso, "MM").unwrap_err().contains("YYYY-MM-DD"));
        }
    }

    #[test]
    fn card_expiry_parts() {
        for (part, given, stored) in [
            ("expMonth", "4", "04"),
            ("expMonth", "12", "12"),
            ("expMonth", "04", "04"),
            ("expYear", "31", "2031"),
            ("expYear", "2031", "2031"),
            ("expMonth", " 4 ", "04"),
        ] {
            assert_eq!(expiry_part(part, given).unwrap(), stored);
        }
        for (part, given) in [
            ("expMonth", "0"),
            ("expMonth", "13"),
            ("expMonth", "Apr"),
            ("expYear", "1"),
            ("expYear", "203"),
            ("expYear", "20311"),
            ("expMonth", "2031"),
        ] {
            assert!(
                expiry_part(part, given)
                    .unwrap_err()
                    .contains("card expiry"),
                "{part} {given}"
            );
        }
        assert_eq!(
            expiry_iso(Some("4"), Some("31")).as_deref(),
            Some("2031-04")
        );
        assert_eq!(
            expiry_iso(Some("04"), Some("2031")).as_deref(),
            Some("2031-04")
        );
        assert_eq!(expiry_iso(Some(""), Some("2031")), None);
        assert_eq!(expiry_iso(Some("04"), None), None);
        assert_eq!(expiry_iso(Some("13"), Some("2031")), None);
    }

    #[test]
    fn a_fields_own_shape_comes_from_its_placeholder_or_length() {
        assert_eq!(
            pattern_of("MM / YY", None, CARD_EXPIRY).as_deref(),
            Some("MM / YY")
        );
        assert_eq!(
            pattern_of("mm/yyyy", None, CARD_EXPIRY).as_deref(),
            Some("MM/YYYY")
        );
        assert_eq!(
            pattern_of("Expiry date", Some(7), CARD_EXPIRY).as_deref(),
            Some("MM/YYYY")
        );
        assert_eq!(
            pattern_of("", Some(4), CARD_EXPIRY).as_deref(),
            Some("MMYY")
        );
        assert_eq!(pattern_of("", None, CARD_EXPIRY), None);
        assert_eq!(
            pattern_of("dd/mm/yyyy", None, DATE_OF_BIRTH).as_deref(),
            Some("DD/MM/YYYY")
        );
    }
}
