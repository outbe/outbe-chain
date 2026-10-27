//! The composite series id.

use super::*;

#[test]
fn the_day_key_still_hashes_to_the_slot_a_bare_u32_would() {
    use outbe_primitives::storage::types::StorageKey;
    assert_eq!(
        WorldwideDay::new(20_260_212).key_bytes(),
        20_260_212u32.key_bytes()
    );
}

#[test]
fn packs_and_unpacks_every_component() {
    let id = SeriesId::pack(DAY, *b"TRY", b'U').unwrap();
    assert_eq!(id.worldwide_day(), DAY);
    assert_eq!(id.as_bytes(), b"20260212-TRY-U");
    assert_eq!(SeriesId::from_bytes(*id.as_bytes()), id);
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
    assert!(SeriesId::pack(WorldwideDay::new(0), *b"USD", b'U').is_err());
    assert!(SeriesId::pack(WorldwideDay::new(100_000_000), *b"USD", b'U').is_err());
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
    let late_a = SeriesId::pack(WorldwideDay::new(DAY.value() + 1), *b"AED", b'A').unwrap();
    assert!(early_z < late_a);

    let same_day_a = SeriesId::pack(DAY, *b"AED", b'U').unwrap();
    assert!(same_day_a < early_z);
}

#[test]
fn round_trips_through_storage_word_and_key() {
    let id = SeriesId::pack(DAY, *b"EUR", b'E').unwrap();
    assert_eq!(SeriesId::from_word(id.to_word()), id);
    assert_eq!(id.key_bytes(), b"20260212-EUR-E".to_vec());
}

#[test]
fn a_series_token_id_round_trips() {
    let id = sid(7);
    assert_eq!(SeriesId::from_token_id(id.token_id()), Some(id));
    assert!(id.token_id() < U256::ONE << 112);
    assert_eq!(SeriesId::from_token_id(U256::ONE << 112), None);
}
