//! Pure gem identities shared by issuance and off-chain calculation.
use alloy_primitives::{B256, U256};
use outbe_primitives::storage::StorageKey;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::wwd_entity_id::WwdEntityId;

/// Bucket key from `(worldwide_day, entry_price_minor, reference_currency)`.
///
/// The currency is part of the preimage because `entry_price_minor` is
/// denominated in it. Two gems that share a day and an entry value in
/// different currencies are priced against different oracle rates. They must
/// not share a bucket.
pub fn bucket_key(
    worldwide_day: WorldwideDay,
    entry_price_minor: U256,
    reference_currency: u16,
) -> B256 {
    use alloy_primitives::keccak256;
    let mut buf = [0u8; 38];
    buf[0..4].copy_from_slice(worldwide_day.key_bytes().as_slice());
    buf[4..36].copy_from_slice(&entry_price_minor.to_be_bytes::<32>());
    buf[36..38].copy_from_slice(&reference_currency.to_be_bytes());
    keccak256(buf)
}

/// Canonical bucket identity `WWD_BE4 || bucket_key`.
#[must_use]
pub fn bucket_id(worldwide_day: WorldwideDay, bucket_key: B256) -> WwdEntityId {
    WwdEntityId::from_day_and_digest(worldwide_day, bucket_key)
}
