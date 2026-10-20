//! Canonical keys for the NOD group-member index.
use alloy_primitives::B256;

/// Storage key for the `index`-th Nod parked in `group_key`.
pub fn group_nod_key(group_key: B256, index: u32) -> B256 {
    let mut buf = [0u8; 36];
    buf[0..32].copy_from_slice(group_key.as_slice());
    buf[32..36].copy_from_slice(&index.to_be_bytes());
    alloy_primitives::keccak256(buf)
}
