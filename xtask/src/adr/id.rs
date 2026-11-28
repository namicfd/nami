//! ADR IDs: the UTC minute an ADR was written in, `YYYYMMDD-HHMM`, or the four-digit number of
//! an ADR written before IDs were minutes, `0000` to `0043`.
//!
//! Every parser of ADR IDs reads them here: tidy's ADR rules, the index, the references check
//! and the `adr clashes` check on each pull request.

use std::fmt;
use std::ops::RangeInclusive;

/// The numbers of the ADRs written before IDs were minutes. Nothing new gets a number: the
/// gaps were never used, and a number above them would clash on parallel branches again.
pub const LEGACY_NUMBERS: [RangeInclusive<u32>; 3] = [0..=15, 17..=23, 32..=43];

/// Digits in a legacy number.
const LEGACY_DIGITS: usize = 4;

/// Digits in the date of a stamp, `YYYYMMDD`.
const DATE_DIGITS: usize = 8;

/// Digits in the time of a stamp, `HHMM`.
const TIME_DIGITS: usize = 4;

/// The earliest year a stamp may name: ADR IDs became minutes in 2026.
const YEAR_MIN: u32 = 2026;

/// Seconds in a day.
const SECONDS_PER_DAY: u64 = 86_400;

/// Seconds from the Unix epoch to 10000-01-01, the first minute a stamp cannot name.
const SECONDS_BEFORE_YEAR_10000: u64 = 253_402_300_800;

/// The ID of one ADR. Legacy numbers sort before stamps, and stamps sort by time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdrId {
    /// `NNNN`: an ADR written before IDs were minutes.
    Legacy(u32),
    /// `YYYYMMDD-HHMM`: the UTC minute the ADR was written in.
    Stamp(Stamp),
}

/// A UTC minute, from 2026 on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
}

/// What an `ADR-` reference names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    /// A well-formed ID, which may or may not have a file.
    Id(AdrId),
    /// Digits that form no ID, such as `123`, or `20261340-1200` with its thirteenth month.
    Malformed,
}

impl Stamp {
    /// The stamp for this calendar minute, or `None` if no such minute exists or it lies
    /// before [`YEAR_MIN`].
    #[must_use]
    pub fn new(year: u32, month: u32, day: u32, hour: u32, minute: u32) -> Option<Self> {
        let is_valid = (YEAR_MIN..=9999).contains(&year)
            && (1..=12).contains(&month)
            && (1..=days_in_month(year, month)).contains(&day)
            && hour < 24
            && minute < 60;
        is_valid.then_some(Self { year, month, day, hour, minute })
    }

    /// The minute that `seconds` after the Unix epoch falls in, or `None` if it lies outside the
    /// years [`YEAR_MIN`] to 9999: the clock is wrong.
    #[must_use]
    pub fn from_unix_seconds(seconds: u64) -> Option<Self> {
        if seconds >= SECONDS_BEFORE_YEAR_10000 {
            return None;
        }
        let (year, month, day) = civil_from_days(seconds / SECONDS_PER_DAY);
        let of_day = seconds % SECONDS_PER_DAY;
        let hour = u32::try_from(of_day / 3600).expect("an hour of the day fits in u32");
        let minute = u32::try_from(of_day % 3600 / 60).expect("a minute fits in u32");
        Self::new(year, month, day, hour, minute)
    }

    /// The minute after this one.
    #[must_use]
    pub fn next_minute(self) -> Self {
        let Self { mut year, mut month, mut day, mut hour, mut minute } = self;
        minute += 1;
        if minute == 60 {
            (minute, hour) = (0, hour + 1);
        }
        if hour == 24 {
            (hour, day) = (0, day + 1);
        }
        if day > days_in_month(year, month) {
            (day, month) = (1, month + 1);
        }
        if month == 13 {
            (month, year) = (1, year + 1);
        }
        let next = Self::new(year, month, day, hour, minute).expect("the next minute exists");
        assert!(next > self, "the next minute is later");
        next
    }

    /// The date as `YYYY-MM-DD`, for an ADR's `Date` line.
    #[must_use]
    pub fn date(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl AdrId {
    /// The ID of an ADR file name: `NNNN-<slug>.md` or `YYYYMMDD-HHMM-<slug>.md`, with any
    /// slug. `None` for any other name. Whether the number is one of [`LEGACY_NUMBERS`] and
    /// the slug well formed is for the rule `adr-id` to say.
    #[must_use]
    pub fn of_file_name(name: &str) -> Option<Self> {
        let stem = name.strip_suffix(".md")?;
        let (id, rest) = parse_prefix(stem)?;
        rest.starts_with('-').then_some(id)
    }

    /// The slug of an ADR file name: what follows the ID and its dash, before `.md`.
    #[must_use]
    pub fn slug_of_file_name(name: &str) -> Option<&str> {
        let stem = name.strip_suffix(".md")?;
        let (_, rest) = parse_prefix(stem)?;
        rest.strip_prefix('-')
    }

    /// True for a stamp, and for a legacy number that an ADR had before IDs were minutes.
    #[must_use]
    pub fn is_assignable(self) -> bool {
        match self {
            Self::Legacy(number) => LEGACY_NUMBERS.iter().any(|range| range.contains(&number)),
            Self::Stamp(_) => true,
        }
    }
}

impl fmt::Display for AdrId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Legacy(number) => write!(formatter, "{number:04}"),
            Self::Stamp(stamp) => write!(
                formatter,
                "{:04}{:02}{:02}-{:02}{:02}",
                stamp.year, stamp.month, stamp.day, stamp.hour, stamp.minute
            ),
        }
    }
}

/// True for a slug: lower-case letters, digits and single dashes between them.
#[must_use]
pub fn is_slug(slug: &str) -> bool {
    slug.split('-').all(|word| {
        !word.is_empty()
            && word.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

/// What the text after `ADR-` names: `None` if it does not start with a digit, so that
/// placeholders such as `ADR-NNNN` are no reference.
#[must_use]
pub fn reference_after(text: &str) -> Option<Reference> {
    if !text.starts_with(|character: char| character.is_ascii_digit()) {
        return None;
    }
    let found = parse_prefix(text).filter(|(_, rest)| !starts_with_digit(rest));
    Some(found.map_or(Reference::Malformed, |(id, _)| Reference::Id(id)))
}

/// Reads an ID at the start of `text`: a stamp, `YYYYMMDD-HHMM`, or else a legacy number of
/// exactly four digits. Returns the ID and the text after it.
fn parse_prefix(text: &str) -> Option<(AdrId, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == DATE_DIGITS {
        let (date, rest) = text.split_at(DATE_DIGITS);
        let time = rest.strip_prefix('-')?;
        let time_digits = time.bytes().take_while(u8::is_ascii_digit).count();
        if time_digits != TIME_DIGITS {
            return None;
        }
        let (time, after) = time.split_at(TIME_DIGITS);
        let field = |part: &str, from: usize, to: usize| part.get(from..to)?.parse::<u32>().ok();
        let stamp = Stamp::new(
            field(date, 0, 4)?,
            field(date, 4, 6)?,
            field(date, 6, 8)?,
            field(time, 0, 2)?,
            field(time, 2, 4)?,
        )?;
        return Some((AdrId::Stamp(stamp), after));
    }
    if digits == LEGACY_DIGITS {
        let (number, rest) = text.split_at(LEGACY_DIGITS);
        return Some((AdrId::Legacy(number.parse().ok()?), rest));
    }
    None
}

fn starts_with_digit(text: &str) -> bool {
    text.starts_with(|character: char| character.is_ascii_digit())
}

/// Days in `month` of `year`, in the Gregorian calendar.
fn days_in_month(year: u32, month: u32) -> u32 {
    let is_leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    match month {
        2 if is_leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The calendar date of day `days` after the Unix epoch (Howard Hinnant's `civil_from_days`,
/// for days on or after the epoch).
fn civil_from_days(days: u64) -> (u32, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 { month_from_march + 3 } else { month_from_march - 9 };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    let narrow = |value: u64| u32::try_from(value).expect("calendar fields fit in u32");
    let date = (narrow(year), narrow(month), narrow(day));
    assert!((1..=12).contains(&date.1) && (1..=31).contains(&date.2), "a calendar date");
    date
}

#[cfg(test)]
mod tests {
    use super::{AdrId, Reference, Stamp, is_slug, reference_after};

    fn stamp(year: u32, month: u32, day: u32, hour: u32, minute: u32) -> Stamp {
        Stamp::new(year, month, day, hour, minute).expect("a valid minute")
    }

    // Goal: both forms of file name give their ID and slug; anything else gives none.
    #[test]
    fn file_names_give_their_id() {
        let of = AdrId::of_file_name;
        assert_eq!(of("0013-lint-table-refinements.md"), Some(AdrId::Legacy(13)));
        assert_eq!(of("20261128-1903-adr-ids.md"), Some(AdrId::Stamp(stamp(2026, 11, 28, 19, 3))));
        assert_eq!(AdrId::slug_of_file_name("20261128-1903-adr-ids.md"), Some("adr-ids"));
        assert_eq!(AdrId::slug_of_file_name("0013-x.md"), Some("x"));
        for name in ["README.md", "13-x.md", "0013_x.md", "abcd-x.md", "0013-x.txt", "00013-x.md"] {
            assert_eq!(of(name), None, "{name}");
        }
        for name in ["20261128-x.md", "20261128-190-x.md", "20261128-1903x.md", "2026112-1903-x.md"]
        {
            assert_eq!(of(name), None, "{name}");
        }
        assert_eq!(of("20261131-1903-x.md"), None, "November has 30 days");
        assert_eq!(of("20261128-2400-x.md"), None, "hours end at 23");
        assert_eq!(of("20261128-1960-x.md"), None, "minutes end at 59");
        assert_eq!(of("20251128-1903-x.md"), None, "stamps start in 2026");
    }

    // Goal: IDs print as they are written, legacy numbers sort before stamps, and stamps sort by
    // time.
    #[test]
    fn ids_print_and_sort() {
        let early = AdrId::Stamp(stamp(2026, 11, 28, 19, 3));
        let late = AdrId::Stamp(stamp(2027, 1, 1, 0, 0));
        assert_eq!(AdrId::Legacy(7).to_string(), "0007");
        assert_eq!(early.to_string(), "20261128-1903");
        assert_eq!(late.to_string(), "20270101-0000");
        assert!(AdrId::Legacy(43) < early && early < late, "legacy, then by time");
        assert!(AdrId::Legacy(7) < AdrId::Legacy(13));
    }

    // Goal: only the numbers that ADRs had before IDs were minutes are assignable, never a gap
    // or a number above them.
    #[test]
    fn legacy_numbers_are_closed() {
        for number in [0, 15, 17, 23, 32, 43] {
            assert!(AdrId::Legacy(number).is_assignable(), "{number}");
        }
        for number in [16, 24, 31, 44, 9999] {
            assert!(!AdrId::Legacy(number).is_assignable(), "{number}");
        }
        assert!(AdrId::Stamp(stamp(2026, 11, 28, 19, 3)).is_assignable());
    }

    // Goal: a minute rolls over into the next hour, day, month and year, and February knows leap
    // years.
    #[test]
    fn the_next_minute_rolls_over() {
        assert_eq!(stamp(2026, 11, 28, 19, 3).next_minute(), stamp(2026, 11, 28, 19, 4));
        assert_eq!(stamp(2026, 11, 28, 19, 59).next_minute(), stamp(2026, 11, 28, 20, 0));
        assert_eq!(stamp(2026, 11, 28, 23, 59).next_minute(), stamp(2026, 11, 29, 0, 0));
        assert_eq!(stamp(2026, 11, 30, 23, 59).next_minute(), stamp(2026, 12, 1, 0, 0));
        assert_eq!(stamp(2026, 12, 31, 23, 59).next_minute(), stamp(2027, 1, 1, 0, 0));
        assert_eq!(stamp(2028, 2, 28, 23, 59).next_minute(), stamp(2028, 2, 29, 0, 0));
        assert_eq!(stamp(2027, 2, 28, 23, 59).next_minute(), stamp(2027, 3, 1, 0, 0));
        assert_eq!(Stamp::new(2100, 2, 29, 0, 0), None, "2100 is no leap year");
        assert!(Stamp::new(2400, 2, 29, 0, 0).is_some(), "2400 is one");
    }

    // Goal: Unix time converts to the UTC minute, across days, months and leap days, and a clock
    // before 2026 or past 9999 gives no stamp instead of a panic.
    #[test]
    fn unix_seconds_give_the_utc_minute() {
        let of = Stamp::from_unix_seconds;
        assert_eq!(of(1_795_892_580), Some(stamp(2026, 11, 28, 19, 3)));
        assert_eq!(of(1_795_892_639), Some(stamp(2026, 11, 28, 19, 3)));
        assert_eq!(of(1_767_225_600), Some(stamp(2026, 1, 1, 0, 0)));
        assert_eq!(of(1_835_395_200), Some(stamp(2028, 2, 29, 0, 0)));
        assert_eq!(of(253_402_300_799), Some(stamp(9999, 12, 31, 23, 59)));
        assert_eq!(of(1_767_225_599), None, "2025");
        assert_eq!(of(0), None, "1970");
        assert_eq!(of(253_402_300_800), None, "10000");
        assert_eq!(of(u64::MAX), None, "far past 9999");
        assert_eq!(stamp(2026, 3, 9, 1, 2).date(), "2026-03-09");
    }

    // Goal: the text after `ADR-` is a reference only if it starts with a digit, and digits that
    // form no ID are malformed.
    #[test]
    fn references_are_read_after_the_prefix() {
        let id = |text: &str| reference_after(text);
        assert_eq!(id("0022's rule"), Some(Reference::Id(AdrId::Legacy(22))));
        assert_eq!(id("0022"), Some(Reference::Id(AdrId::Legacy(22))));
        let stamped = AdrId::Stamp(stamp(2026, 11, 28, 19, 3));
        assert_eq!(id("20261128-1903, point 2"), Some(Reference::Id(stamped)));
        assert_eq!(id("NNNN"), None, "a placeholder");
        assert_eq!(id("YYYYMMDD-HHMM"), None, "a placeholder");
        for malformed in ["123", "00221", "20261128", "20261128-19031", "20261340-1200"] {
            assert_eq!(id(malformed), Some(Reference::Malformed), "{malformed}");
        }
    }

    // Goal: a slug is lower-case words joined by single dashes.
    #[test]
    fn slugs_are_lower_case_words() {
        for slug in ["adr-ids", "x", "a1-b2"] {
            assert!(is_slug(slug), "{slug}");
        }
        for slug in ["", "-a", "a-", "a--b", "Adr", "a_b", "a b", "a.md"] {
            assert!(!is_slug(slug), "{slug}");
        }
    }
}
