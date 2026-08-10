//! Composite series identifier.
//!
//! A series is one currency pair of one worldwide day's auction, so its id packs
//! all three into a single word: the day, the issuance currency and the reference
//! currency. The currency codes travel as ASCII inside the id because a target
//! chain has no oracle to resolve them — a series has to describe itself wherever
//! it lands.

use alloy_primitives::U256;
use outbe_primitives::storage::types::{Storable, StorableType, StorageKey};
use std::fmt;

use crate::errors::IntexError;

/// Packed series identifier: `worldwide_day (u32) | issuance (3 bytes) | reference (1 byte)`.
///
/// Codes are alpha-3 (`USD`) when the oracle knows them and the zero-padded
/// numeric code (`949`) when it does not; the reference byte is the first byte of
/// its own three, so one rule covers both forms.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeriesId(u64);

impl SeriesId {
    /// Packs the three components. Rejects a zero day and any code byte outside
    /// `A-Z` / `0-9`, so an unpacked id always renders.
    pub fn pack(worldwide_day: u32, issuance: [u8; 3], reference: u8) -> Result<Self, IntexError> {
        if worldwide_day == 0 {
            return Err(IntexError::InvalidSeriesId);
        }
        for byte in issuance {
            if !is_code_byte(byte) {
                return Err(IntexError::InvalidSeriesId);
            }
        }
        if !is_code_byte(reference) {
            return Err(IntexError::InvalidSeriesId);
        }
        Ok(Self(
            (u64::from(worldwide_day) << 32)
                | (u64::from(issuance[0]) << 24)
                | (u64::from(issuance[1]) << 16)
                | (u64::from(issuance[2]) << 8)
                | u64::from(reference),
        ))
    }

    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }

    pub const fn worldwide_day(self) -> u32 {
        (self.0 >> 32) as u32
    }

    pub const fn issuance_code(self) -> [u8; 3] {
        [
            (self.0 >> 24) as u8,
            (self.0 >> 16) as u8,
            (self.0 >> 8) as u8,
        ]
    }

    pub const fn reference_code(self) -> u8 {
        self.0 as u8
    }

    /// The three ASCII digits of an ISO numeric code, used when the oracle holds
    /// no alpha code for it: `949 -> b"949"`, `32 -> b"032"`.
    pub fn numeric_code(iso: u16) -> [u8; 3] {
        let iso = iso % 1000;
        [
            b'0' + (iso / 100) as u8,
            b'0' + ((iso / 10) % 10) as u8,
            b'0' + (iso % 10) as u8,
        ]
    }
}

const fn is_code_byte(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit()
}

impl fmt::Display for SeriesId {
    /// `20260212-TRY-U`, or `20260212-949-U` when the issuance code is numeric.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let issuance = self.issuance_code();
        write!(
            f,
            "{}-{}{}{}-{}",
            self.worldwide_day(),
            issuance[0] as char,
            issuance[1] as char,
            issuance[2] as char,
            self.reference_code() as char,
        )
    }
}

impl StorableType for SeriesId {
    const SLOTS: usize = 1;
}

impl Storable for SeriesId {
    fn from_word(word: U256) -> Self {
        Self(word.to::<u64>())
    }

    fn to_word(&self) -> U256 {
        U256::from(self.0)
    }
}

impl StorageKey for SeriesId {
    fn key_bytes(&self) -> Vec<u8> {
        self.0.to_be_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u32 = 20_260_212;

    #[test]
    fn packs_and_unpacks_every_component() {
        let id = SeriesId::pack(DAY, *b"TRY", b'U').unwrap();
        assert_eq!(id.worldwide_day(), DAY);
        assert_eq!(id.issuance_code(), *b"TRY");
        assert_eq!(id.reference_code(), b'U');
        assert_eq!(SeriesId::from_raw(id.value()), id);
    }

    #[test]
    fn renders_alpha_and_numeric_forms() {
        assert_eq!(
            SeriesId::pack(DAY, *b"TRY", b'U').unwrap().to_string(),
            "20260212-TRY-U"
        );
        assert_eq!(
            SeriesId::pack(DAY, SeriesId::numeric_code(949), b'U')
                .unwrap()
                .to_string(),
            "20260212-949-U"
        );
    }

    #[test]
    fn numeric_code_zero_pads() {
        assert_eq!(SeriesId::numeric_code(840), *b"840");
        assert_eq!(SeriesId::numeric_code(32), *b"032");
        assert_eq!(SeriesId::numeric_code(8), *b"008");
    }

    #[test]
    fn rejects_a_zero_day_and_lowercase_or_symbol_codes() {
        assert!(SeriesId::pack(0, *b"USD", b'U').is_err());
        assert!(SeriesId::pack(DAY, *b"usd", b'U').is_err());
        assert!(SeriesId::pack(DAY, *b"US-", b'U').is_err());
        assert!(SeriesId::pack(DAY, *b"USD", b'u').is_err());
        assert!(SeriesId::pack(DAY, *b"USD", 0).is_err());
    }

    #[test]
    fn orders_by_day_before_currency() {
        // Dense enumeration and the bin-tree range scans both walk ids in order,
        // so a later day must never sort before an earlier one.
        let early_z = SeriesId::pack(DAY, *b"ZWL", b'Z').unwrap();
        let late_a = SeriesId::pack(DAY + 1, *b"AED", b'A').unwrap();
        assert!(early_z < late_a);

        let same_day_a = SeriesId::pack(DAY, *b"AED", b'U').unwrap();
        assert!(same_day_a < early_z);
    }

    #[test]
    fn round_trips_through_storage_word_and_key() {
        let id = SeriesId::pack(DAY, *b"EUR", b'E').unwrap();
        assert_eq!(SeriesId::from_word(id.to_word()), id);
        assert_eq!(id.key_bytes(), id.value().to_be_bytes().to_vec());
    }
}
