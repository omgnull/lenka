// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

use std::fmt;

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::macros::{date, format_description};
use time::{Date, OffsetDateTime, UtcOffset};

use crate::error::{LenkaError, Result};

/// A point in time, with millisecond precision. The text form is RFC 3339 in UTC.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Timestamp(i64);

/// The first time that the text form can hold: 0000-01-01T00:00:00Z.
const FIRST_UNIX_MS: i64 = -62_167_219_200_000;
/// The last time that the text form can hold: 9999-12-31T23:59:59.999Z.
const LAST_UNIX_MS: i64 = 253_402_300_799_999;

impl Timestamp {
    /// Returns the current system time.
    #[must_use]
    pub fn now() -> Self {
        let nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
        Self(i64::try_from(nanos / 1_000_000).unwrap_or(i64::MAX))
    }

    /// Creates a timestamp from milliseconds after the Unix epoch.
    #[must_use]
    pub fn from_unix_ms(ms: i64) -> Self {
        Self(ms.clamp(FIRST_UNIX_MS, LAST_UNIX_MS))
    }

    /// Returns the milliseconds after the Unix epoch.
    #[must_use]
    pub fn unix_ms(self) -> i64 {
        self.0
    }

    /// Returns this time plus some milliseconds.
    #[must_use]
    pub fn plus_ms(self, ms: i64) -> Self {
        Self::from_unix_ms(self.0.saturating_add(ms))
    }

    /// Parses the RFC 3339 form.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InvalidArgument`] if the text is not valid.
    pub fn parse(text: &str) -> Result<Self> {
        let value = OffsetDateTime::parse(text, &Rfc3339)
            .map_err(|_| LenkaError::invalid_argument("The time is not valid RFC 3339."))?;
        let ms = i64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX);
        if !(FIRST_UNIX_MS..=LAST_UNIX_MS).contains(&ms) {
            return Err(LenkaError::invalid_argument(
                "The time must be in the years 0000 to 9999.",
            ));
        }
        Ok(Self(ms))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = i128::from(self.0) * 1_000_000;
        let value = OffsetDateTime::from_unix_timestamp_nanos(nanos)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
            .to_offset(UtcOffset::UTC);
        match value.format(&Rfc3339) {
            Ok(text) => f.write_str(&text),
            Err(_) => f.write_str("1970-01-01T00:00:00Z"),
        }
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({self})")
    }
}

impl TryFrom<String> for Timestamp {
    type Error = LenkaError;

    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<Timestamp> for String {
    fn from(value: Timestamp) -> Self {
        value.to_string()
    }
}

/// A calendar day. The text form is `YYYY-MM-DD`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Day(Date);

/// The day number of the first day that the text form can show.
const FIRST_DAY_NUMBER: i32 = date!(0000 - 01 - 01).to_julian_day();
/// The day number of the last day that the text form can show.
const LAST_DAY_NUMBER: i32 = date!(9999 - 12 - 31).to_julian_day();

impl Day {
    /// Parses the `YYYY-MM-DD` form.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InvalidArgument`] if the text is not valid.
    pub fn parse(text: &str) -> Result<Self> {
        // Only the years 0 to 9999 have four digits, and each saved day must parse again.
        Date::parse(text, format_description!("[year]-[month]-[day]"))
            .ok()
            .filter(|date| date.year() >= 0)
            .map(Self)
            .filter(|day| day.to_string() == text)
            .ok_or_else(|| LenkaError::invalid_argument("A date must have the form YYYY-MM-DD."))
    }

    /// Returns today in UTC.
    #[must_use]
    pub fn today() -> Self {
        Self(OffsetDateTime::now_utc().date())
    }

    /// Returns the day number, for date calculations. Day numbers are consecutive.
    #[must_use]
    pub fn number(self) -> i32 {
        self.0.to_julian_day()
    }

    /// Creates a day from a day number. A number outside the years 0 to 9999 gives the nearest
    /// day in these years.
    #[must_use]
    pub fn from_number(number: i32) -> Self {
        let number = number.clamp(FIRST_DAY_NUMBER, LAST_DAY_NUMBER);
        Self(Date::from_julian_day(number).unwrap_or(OffsetDateTime::UNIX_EPOCH.date()))
    }

    /// Returns the day of the month, from 1 to 31.
    #[must_use]
    pub fn day_of_month(self) -> u8 {
        self.0.day()
    }

    /// Returns the month, from 1 to 12.
    #[must_use]
    pub fn month(self) -> u8 {
        u8::from(self.0.month())
    }

    /// Returns the year.
    #[must_use]
    pub fn year(self) -> i32 {
        self.0.year()
    }
}

impl fmt::Display for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}",
            self.0.year(),
            u8::from(self.0.month()),
            self.0.day()
        )
    }
}

impl fmt::Debug for Day {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Day({self})")
    }
}

impl TryFrom<String> for Day {
    type Error = LenkaError;

    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<Day> for String {
    fn from(value: Day) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_keeps_its_value_in_a_round_trip() {
        for ms in [i64::MAX, i64::MIN, FIRST_UNIX_MS, LAST_UNIX_MS, 0] {
            let time = Timestamp::from_unix_ms(ms);
            assert_eq!(Timestamp::parse(&time.to_string()).ok(), Some(time), "{ms}");
        }
        let far = Timestamp::from_unix_ms(LAST_UNIX_MS).plus_ms(i64::MAX);
        assert_eq!(far.unix_ms(), LAST_UNIX_MS);
        assert!(Timestamp::parse("9999-12-31T23:59:59-23:59").is_err());
    }

    #[test]
    fn timestamp_round_trip() {
        let value = Timestamp::from_unix_ms(1_793_000_000_123);
        assert_eq!(Timestamp::parse(&value.to_string()).ok(), Some(value));
    }

    #[test]
    fn each_valid_day_can_be_read_again() {
        for bad in [
            "-0001-01-01",
            "-0999-12-31",
            "-1000-01-01",
            "-001-01-01",
            "+0001-01-01",
        ] {
            assert!(Day::parse(bad).is_err(), "{bad}");
        }
        for good in ["0000-01-01", "0001-02-03", "9999-12-31"] {
            let day = Day::parse(good).expect("a valid day");
            assert_eq!(day.to_string(), good);
            assert_eq!(Day::from_number(day.number()), day);
        }
    }

    #[test]
    fn a_day_number_outside_the_years_gives_the_nearest_day() {
        assert_eq!(Day::from_number(i32::MAX).to_string(), "9999-12-31");
        assert_eq!(Day::from_number(i32::MIN).to_string(), "0000-01-01");
    }

    #[test]
    fn day_round_trip_and_numbers() {
        let day = Day::parse("2026-11-06").ok();
        assert_eq!(day.map(|d| d.to_string()).as_deref(), Some("2026-11-06"));
        let day = Day::parse("2026-12-31").ok();
        let next = day.map(|d| Day::from_number(d.number() + 1).to_string());
        assert_eq!(next.as_deref(), Some("2027-01-01"));
        assert!(Day::parse("2026-13-01").is_err());
    }
}
