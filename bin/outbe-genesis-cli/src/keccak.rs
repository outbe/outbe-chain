//! Ethereum Keccak-256 hashing helper.

/// Compute the 32-byte Ethereum Keccak-256 digest of `data`.
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    alloy_primitives::keccak256(data).0
}
