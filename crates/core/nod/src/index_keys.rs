//! Canonical keys for the NOD call-bin and bucket-member indexes.
use alloy_primitives::B256;

/// Namespaces a bin-column key by the bucket's reference currency.
///
/// Mapping keys are left-padded to 32 bytes before hashing, so a wider
/// integer type alone namespaces nothing. The ISO has to occupy real
/// high bits. Bin ids are 24-bit and the trie's mid/leaf keys are 16-bit,
/// so the low 32 bits always hold `key` unambiguously. ISO `0` is the one
/// value that would alias the un-namespaced key. `record_nod_issued`
/// rejects it at the funnel so it can never be written.
pub(crate) const fn scoped(reference_currency: u16, key: u32) -> u64 {
    ((reference_currency as u64) << 32) | key as u64
}

/// Storage key for the `index`-th bucket_key parked in bin `bin_id` of
/// `reference_currency`. Mirrors the `owner_index_key` keccak-of-concat
/// pattern.
pub(crate) fn bin_index_key(reference_currency: u16, bin_id: u32, index: u32) -> B256 {
    let mut buf = [0u8; 10];
    buf[0..2].copy_from_slice(&reference_currency.to_be_bytes());
    buf[2..6].copy_from_slice(&bin_id.to_be_bytes());
    buf[6..10].copy_from_slice(&index.to_be_bytes());
    alloy_primitives::keccak256(buf)
}

/// Storage key for the `index`-th Nod parked in `bucket_key`.
pub(crate) fn bucket_nod_key(bucket_key: B256, index: u32) -> B256 {
    let mut buf = [0u8; 36];
    buf[0..32].copy_from_slice(bucket_key.as_slice());
    buf[32..36].copy_from_slice(&index.to_be_bytes());
    alloy_primitives::keccak256(buf)
}
