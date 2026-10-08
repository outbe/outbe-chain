//! Canonical keys for the NOD bucket-member index.
use alloy_primitives::B256;

/// Storage key for the `index`-th Nod parked in `bucket_key`.
pub(crate) fn bucket_nod_key(bucket_key: B256, index: u32) -> B256 {
    let mut buf = [0u8; 36];
    buf[0..32].copy_from_slice(bucket_key.as_slice());
    buf[32..36].copy_from_slice(&index.to_be_bytes());
    alloy_primitives::keccak256(buf)
}
