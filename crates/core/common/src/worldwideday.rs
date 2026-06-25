use alloy_primitives::U256;
use outbe_primitives::storage::types::{Storable, StorableType, StorageKey};
use outbe_primitives::time::{
    date_key_to_utc_timestamp, timestamp_to_date_key, UTC_PLUS_14_OFFSET,
};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use time::{Date, Month};

/// Typed worldwide day identifier in YYYYMMDD format.
#[repr(transparent)]
#[derive(
    Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct WorldwideDay(u32);

impl WorldwideDay {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u32 {
        self.0
    }

    /// Returns true if this value encodes a valid YYYYMMDD calendar date.
    pub fn is_valid(self) -> bool {
        let (year, month, day) = self.parse_wwd_to_nums();
        let Ok(month) = Month::try_from(month) else {
            return false;
        };
        Date::from_calendar_date(year, month, day).is_ok()
    }

    fn parse_wwd_to_nums(self) -> (i32, u8, u8) {
        let raw = self.0;
        let year = (raw / 10_000) as i32;
        let month = ((raw / 100) % 100) as u8;
        let day = (raw % 100) as u8;
        (year, month, day)
    }

    /// Returns the worldwide day key for a unix timestamp (UTC+14).
    pub fn from_timestamp(timestamp: u64) -> Self {
        Self(timestamp_to_date_key(timestamp + UTC_PLUS_14_OFFSET))
    }

    /// Returns the forming-start timestamp for this worldwide day.
    pub fn start_timestamp(self) -> u64 {
        date_key_to_utc_timestamp(self.0).saturating_sub(UTC_PLUS_14_OFFSET)
    }

    /// Returns the previous calendar day.
    pub fn previous_date_key(self) -> Self {
        Self(outbe_primitives::time::previous_date_key(self.0))
    }

    /// Returns the WWD in UNIX timestamp seconds.
    pub fn to_timestamp_utc(self) -> u64 {
        date_key_to_utc_timestamp(self.0)
    }
}

impl fmt::Display for WorldwideDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u32> for WorldwideDay {
    fn from(value: u32) -> Self {
        Self::new(value)
    }
}

impl From<WorldwideDay> for u32 {
    fn from(value: WorldwideDay) -> Self {
        value.value()
    }
}

impl FromStr for WorldwideDay {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s
            .parse::<u32>()
            .map_err(|_| "worldwide_day must be a valid u32 value".to_string())?;
        let value = Self::new(raw);
        if !value.is_valid() {
            return Err("worldwide_day must be a valid YYYYMMDD date".to_string());
        }
        Ok(value)
    }
}

/// Storage implementation for WorldwideDay as a single 32-bit word.
impl StorableType for WorldwideDay {
    const SLOTS: usize = 1;
}

impl Storable for WorldwideDay {
    fn from_word(word: U256) -> Self {
        Self(word.to::<u32>())
    }

    fn to_word(&self) -> U256 {
        U256::from(self.0)
    }
}

impl StorageKey for WorldwideDay {
    fn key_bytes(&self) -> Vec<u8> {
        self.0.to_be_bytes().to_vec()
    }
}
