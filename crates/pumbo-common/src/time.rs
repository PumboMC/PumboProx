//! Durations typed by admins (`30m`, `1d2h30m`, `2w`, `perm`) and their display.
//!
//! Units: `s` seconds, `m` minutes, `h` hours, `d` days, `w` weeks, `mo` months
//! (30 days), `y` years (365 days). Units are case-insensitive and parts may be
//! separated by spaces (`1d 12h`). `perm` or `permanent` means no end.

use std::fmt;
use std::time::Duration;

const UNITS: &[(&str, u64)] =
    &[("mo", 30 * 86_400), ("y", 365 * 86_400), ("w", 7 * 86_400), ("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)];

/// How long something lasts: forever or for a duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Term {
    Permanent,
    Limited(Duration),
}

impl Term {
    pub fn is_permanent(self) -> bool {
        matches!(self, Term::Permanent)
    }

    /// End time in Unix milliseconds, `None` for permanent.
    pub fn expires_at(self, now_ms: u64) -> Option<u64> {
        match self {
            Term::Permanent => None,
            Term::Limited(d) => Some(now_ms.saturating_add(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))),
        }
    }
}

impl fmt::Display for Term {
    /// `permanent` or the duration as [`format_duration`] writes it. Translated
    /// text comes from the `time-permanent` message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Term::Permanent => f.write_str("permanent"),
            Term::Limited(d) => f.write_str(&format_duration(*d)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeError {
    Empty,
    /// A number without a unit, such as `30`.
    MissingUnit(String),
    UnknownUnit(String),
    /// Something that is not a number where one was expected.
    BadNumber(String),
    TooLong,
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimeError::Empty => f.write_str("empty duration"),
            TimeError::MissingUnit(n) => write!(f, "missing unit after {n} (use s, m, h, d, w, mo or y)"),
            TimeError::UnknownUnit(u) => write!(f, "unknown time unit '{u}' (use s, m, h, d, w, mo or y)"),
            TimeError::BadNumber(t) => write!(f, "'{t}' is not a duration"),
            TimeError::TooLong => f.write_str("duration is too long"),
        }
    }
}

impl std::error::Error for TimeError {}

/// Parses `1d2h30m` style durations. Zero (`0s`) is allowed.
pub fn parse_duration(text: &str) -> Result<Duration, TimeError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(TimeError::Empty);
    }
    let mut total: u64 = 0;
    let mut rest = text;
    while !rest.is_empty() {
        rest = rest.trim_start();
        let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let (number, after) = rest.split_at(digits);
        if number.is_empty() {
            return Err(TimeError::BadNumber(rest.to_string()));
        }
        let value: u64 = number.parse().map_err(|_| TimeError::TooLong)?;
        let unit_len = after.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(after.len());
        let (unit, after) = after.split_at(unit_len);
        if unit.is_empty() {
            return Err(TimeError::MissingUnit(number.to_string()));
        }
        let lower = unit.to_ascii_lowercase();
        let Some((_, secs)) = UNITS.iter().find(|(name, _)| *name == lower) else {
            return Err(TimeError::UnknownUnit(unit.to_string()));
        };
        let part = value.checked_mul(*secs).ok_or(TimeError::TooLong)?;
        total = total.checked_add(part).ok_or(TimeError::TooLong)?;
        rest = after;
    }
    // Keep end times representable in milliseconds.
    if total > u64::MAX / 1000 {
        return Err(TimeError::TooLong);
    }
    Ok(Duration::from_secs(total))
}

/// Parses a duration or `perm` / `permanent`.
pub fn parse_term(text: &str) -> Result<Term, TimeError> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("perm") || t.eq_ignore_ascii_case("permanent") {
        return Ok(Term::Permanent);
    }
    parse_duration(t).map(Term::Limited)
}

/// Writes a duration as `1d 2h 30m 5s` (days, hours, minutes, seconds; zero
/// parts left out; `0s` for less than a second). Parses back with [`parse_duration`].
pub fn format_duration(d: Duration) -> String {
    format_duration_coarse(d, usize::MAX)
}

/// Like [`format_duration`] but with at most `parts` parts, cut off (not
/// rounded): `format_duration_coarse(1d 2h 30m, 2)` is `1d 2h`.
pub fn format_duration_coarse(d: Duration, parts: usize) -> String {
    let mut secs = d.as_secs();
    let mut out: Vec<String> = Vec::new();
    for (unit, size) in [("d", 86_400u64), ("h", 3_600), ("m", 60), ("s", 1)] {
        if out.len() >= parts.max(1) {
            break;
        }
        let n = secs / size;
        secs %= size;
        if n > 0 {
            out.push(format!("{n}{unit}"));
        }
    }
    if out.is_empty() { "0s".to_string() } else { out.join(" ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn parses_units_and_combinations() {
        assert_eq!(parse_duration("30m"), Ok(secs(1800)));
        assert_eq!(parse_duration("1d2h30m"), Ok(secs(86_400 + 7_200 + 1_800)));
        assert_eq!(parse_duration(" 1d 12H "), Ok(secs(86_400 + 43_200)));
        assert_eq!(parse_duration("2w"), Ok(secs(14 * 86_400)));
        assert_eq!(parse_duration("1mo"), Ok(secs(30 * 86_400)));
        assert_eq!(parse_duration("1y1m"), Ok(secs(365 * 86_400 + 60)));
        assert_eq!(parse_duration("0s"), Ok(secs(0)));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(parse_duration(""), Err(TimeError::Empty));
        assert_eq!(parse_duration("30"), Err(TimeError::MissingUnit("30".into())));
        assert_eq!(parse_duration("5x"), Err(TimeError::UnknownUnit("x".into())));
        assert_eq!(parse_duration("d5"), Err(TimeError::BadNumber("d5".into())));
        assert_eq!(parse_duration("1d-2h"), Err(TimeError::BadNumber("-2h".into())));
        assert_eq!(parse_duration("99999999999999999999y"), Err(TimeError::TooLong));
        assert_eq!(parse_duration("999999999999999y"), Err(TimeError::TooLong));
        assert!(parse_term("never").is_err());
    }

    #[test]
    fn permanent() {
        assert_eq!(parse_term("perm"), Ok(Term::Permanent));
        assert_eq!(parse_term("PERMANENT"), Ok(Term::Permanent));
        assert_eq!(parse_term("1h"), Ok(Term::Limited(secs(3600))));
        assert_eq!(Term::Permanent.expires_at(5), None);
        assert_eq!(Term::Limited(secs(2)).expires_at(1_000), Some(3_000));
        assert!(Term::Permanent.is_permanent());
        assert_eq!(Term::Permanent.to_string(), "permanent");
    }

    #[test]
    fn formats_and_round_trips() {
        assert_eq!(format_duration(secs(0)), "0s");
        assert_eq!(format_duration(Duration::from_millis(999)), "0s");
        assert_eq!(format_duration(secs(90_061)), "1d 1h 1m 1s");
        assert_eq!(format_duration(secs(14 * 86_400)), "14d");
        assert_eq!(format_duration_coarse(secs(86_400 + 7_200 + 1_800), 2), "1d 2h");
        assert_eq!(format_duration_coarse(secs(59), 0), "59s");
        for s in [1u64, 59, 61, 3_600, 90_061, 400 * 86_400] {
            assert_eq!(parse_duration(&format_duration(secs(s))), Ok(secs(s)));
        }
        assert_eq!(Term::Limited(secs(3_660)).to_string(), "1h 1m");
    }
}
