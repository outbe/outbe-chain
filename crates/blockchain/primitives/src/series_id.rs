//! The series id shared by the rights. `YYYYMMDD-III-R` names the issuance code and the
//! reference letter when the two currencies differ; `YYYYMMDD-CCC` names the one currency
//! when they match, padded with two `0x00` bytes in the 14-byte field.

use alloy_primitives::{FixedBytes, U256};
use std::fmt;

use crate::stablecoin::iso_4217_alpha;
use crate::storage::types::{Storable, StorableType, StorageKey};

pub const SERIES_ID_LEN: usize = 14;

const DAY_DIGITS: usize = 8;
const ISSUANCE_AT: usize = 9;
const REFERENCE_AT: usize = 13;
const SHORT_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid series id components")]
pub struct InvalidSeriesId;

/// Series identifier: `20260212-TRY-U`, or `20261102-EUR` plus two `0x00` bytes.
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeriesId([u8; SERIES_ID_LEN]);

impl SeriesId {
    /// Rejects a day outside eight digits and code bytes outside `A-Z` / `0-9`.
    pub fn pack(mut day: u32, issuance: [u8; 3], reference: u8) -> Result<Self, InvalidSeriesId> {
        if day == 0 || day > 99_999_999 {
            return Err(InvalidSeriesId);
        }
        if !issuance.iter().all(|byte| is_code_byte(*byte)) || !is_code_byte(reference) {
            return Err(InvalidSeriesId);
        }
        let mut bytes = [b'-'; SERIES_ID_LEN];
        for slot in bytes[..DAY_DIGITS].iter_mut().rev() {
            *slot = b'0' + (day % 10) as u8;
            day /= 10;
        }
        bytes[ISSUANCE_AT..ISSUANCE_AT + 3].copy_from_slice(&issuance);
        bytes[REFERENCE_AT] = reference;
        Ok(Self(bytes))
    }

    /// The one-currency form: `YYYYMMDD-CCC` with two `0x00` bytes at the end.
    pub fn pack_same(day: u32, code: [u8; 3]) -> Result<Self, InvalidSeriesId> {
        let mut id = Self::pack(day, code, b'0')?;
        id.0[SHORT_LEN..].fill(0);
        Ok(id)
    }

    pub const fn from_bytes(bytes: [u8; SERIES_ID_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; SERIES_ID_LEN] {
        &self.0
    }

    /// The ERC-1155 token id of the series' units: `uint256(uint112(seriesId))`.
    pub fn token_id(&self) -> U256 {
        U256::from_be_slice(&self.0)
    }

    /// The series a token id names; `None` above the 14-byte id space.
    pub fn from_token_id(id: U256) -> Option<Self> {
        let word = id.to_be_bytes::<32>();
        let (high, low) = word.split_at(32 - SERIES_ID_LEN);
        if high.iter().any(|byte| *byte != 0) {
            return None;
        }
        let mut bytes = [0u8; SERIES_ID_LEN];
        bytes.copy_from_slice(low);
        Some(Self(bytes))
    }

    /// Id of a right in this series: keccak256 of the 14 id bytes, then `parts` at their fixed
    /// widths. With no parts it is the id of the series itself.
    pub fn right_id(&self, parts: &[&[u8]]) -> U256 {
        let mut hasher = alloy_primitives::Keccak256::new();
        hasher.update(self.0);
        for part in parts {
            hasher.update(part);
        }
        U256::from_be_bytes(hasher.finalize().0)
    }

    /// The date the id leads with, as `YYYYMMDD`.
    pub fn day(&self) -> u32 {
        self.0[..DAY_DIGITS].iter().fold(0u32, |acc, byte| {
            acc * 10 + u32::from(byte.wrapping_sub(b'0'))
        })
    }

    /// How a currency is spelled inside an id: its ISO 4217 alpha-3 code.
    pub fn currency_code(iso: u16) -> Result<[u8; 3], InvalidSeriesId> {
        iso_4217_alpha(iso).ok_or(InvalidSeriesId)
    }

    /// The id of the series a day issues for one `(issuance, reference)` pair.
    pub fn for_pair(day: u32, issuance: u16, reference: u16) -> Result<Self, InvalidSeriesId> {
        let code = Self::currency_code(issuance)?;
        if issuance == reference {
            return Self::pack_same(day, code);
        }
        Self::pack(day, code, Self::currency_code(reference)?[0])
    }
}

const fn is_code_byte(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit()
}

impl fmt::Display for SeriesId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let end = self
            .0
            .iter()
            .rposition(|byte| *byte != 0)
            .map_or(0, |at| at + 1);
        f.write_str(&String::from_utf8_lossy(&self.0[..end]))
    }
}

impl StorableType for SeriesId {
    const SLOTS: usize = 1;
}

/// Left-aligned in the word, matching the Solidity `bytes14` layout.
impl Storable for SeriesId {
    fn from_word(word: U256) -> Self {
        let mut bytes = [0u8; SERIES_ID_LEN];
        bytes.copy_from_slice(&word.to_be_bytes::<32>()[..SERIES_ID_LEN]);
        Self(bytes)
    }

    fn to_word(&self) -> U256 {
        let mut word = [0u8; 32];
        word[..SERIES_ID_LEN].copy_from_slice(&self.0);
        U256::from_be_bytes(word)
    }
}

impl StorageKey for SeriesId {
    fn key_bytes(&self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl crate::deadline_queue::QueueEntry for SeriesId {
    fn is_vacant(&self) -> bool {
        self.0 == [0; SERIES_ID_LEN]
    }
}

impl From<SeriesId> for FixedBytes<SERIES_ID_LEN> {
    fn from(id: SeriesId) -> Self {
        Self(id.0)
    }
}

impl From<FixedBytes<SERIES_ID_LEN>> for SeriesId {
    fn from(value: FixedBytes<SERIES_ID_LEN>) -> Self {
        Self(value.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u32 = 20_260_212;

    #[test]
    fn packs_and_unpacks_every_component() {
        let id = SeriesId::pack(DAY, *b"TRY", b'U').unwrap();
        assert_eq!(id.day(), DAY);
        assert_eq!(id.as_bytes(), b"20260212-TRY-U");
        assert_eq!(SeriesId::from_bytes(*id.as_bytes()), id);
    }

    #[test]
    fn a_right_id_hashes_the_id_bytes_and_its_parts() {
        let id = SeriesId::pack(DAY, *b"TRY", b'U').unwrap();
        let owner = alloy_primitives::Address::repeat_byte(0x11);
        let block = 7u64.to_be_bytes();
        assert_eq!(
            id.right_id(&[]),
            U256::from_be_bytes(alloy_primitives::keccak256(id.as_bytes()).0)
        );
        let mut preimage = id.as_bytes().to_vec();
        preimage.extend_from_slice(owner.as_slice());
        preimage.extend_from_slice(&block);
        assert_eq!(
            id.right_id(&[owner.as_slice(), &block]),
            U256::from_be_bytes(alloy_primitives::keccak256(&preimage).0)
        );
        let other = SeriesId::pack(DAY + 1, *b"TRY", b'U').unwrap();
        assert_ne!(
            other.right_id(&[owner.as_slice()]),
            id.right_id(&[owner.as_slice()])
        );
    }

    #[test]
    fn reads_as_text() {
        assert_eq!(
            SeriesId::pack(DAY, *b"TRY", b'U').unwrap().to_string(),
            "20260212-TRY-U"
        );
    }

    #[test]
    fn for_pair_spells_iso_codes_and_refuses_the_rest() {
        assert_eq!(
            SeriesId::for_pair(DAY, 32, 840).unwrap().as_bytes(),
            b"20260212-ARS-U"
        );
        assert!(SeriesId::for_pair(DAY, 975, 840).is_err());
        assert!(SeriesId::for_pair(DAY, 840, 975).is_err());
        assert!(SeriesId::for_pair(DAY, 1949, 840).is_err());
    }

    #[test]
    fn rejects_a_zero_day_and_lowercase_or_symbol_codes() {
        assert!(SeriesId::pack(0, *b"USD", b'U').is_err());
        assert!(SeriesId::pack(100_000_000, *b"USD", b'U').is_err());
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
    fn one_currency_takes_the_short_form_with_zero_padding() {
        let id = SeriesId::for_pair(DAY, 840, 840).unwrap();
        assert_eq!(id.as_bytes(), b"20260212-USD\0\0");
        assert_eq!(id.to_string(), "20260212-USD");
        assert_eq!(id.day(), DAY);
        assert_eq!(SeriesId::pack_same(DAY, *b"USD").unwrap(), id);
        assert_ne!(id, SeriesId::pack(DAY, *b"USD", b'U').unwrap());
        assert!(SeriesId::pack_same(DAY, *b"usd").is_err());
    }

    #[test]
    fn a_series_token_id_round_trips() {
        let id = SeriesId::pack(DAY, *b"EUR", b'E').unwrap();
        assert_eq!(SeriesId::from_token_id(id.token_id()), Some(id));
        assert!(id.token_id() < U256::ONE << 112);
        assert_eq!(SeriesId::from_token_id(U256::ONE << 112), None);
    }

    #[test]
    fn round_trips_through_storage_word_and_key() {
        let id = SeriesId::pack(DAY, *b"EUR", b'E').unwrap();
        assert_eq!(SeriesId::from_word(id.to_word()), id);
        assert_eq!(id.key_bytes(), b"20260212-EUR-E".to_vec());
    }
}
