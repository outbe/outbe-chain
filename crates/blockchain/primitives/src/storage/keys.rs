//! Composite mapping keys that system and core contracts share.

use alloy_primitives::{keccak256, Address, B256};

/// Returns `keccak256(address || value_be)`, the key of a mapping entry for
/// one address and one 32-bit value.
///
/// The preimage is 24 bytes: the 20 address bytes, then the 4 value bytes in
/// big-endian order. Stored keys depend on this layout, so do not change it.
pub fn address_u32_key(address: Address, value: u32) -> B256 {
    let mut preimage = [0u8; 24];
    preimage[..20].copy_from_slice(address.as_slice());
    preimage[20..].copy_from_slice(&value.to_be_bytes());
    keccak256(preimage)
}

#[cfg(test)]
mod tests {
    use super::address_u32_key;
    use alloy_primitives::{address, b256};

    /// The expected digests come from an independent Keccak-256 of the 24-byte
    /// preimage.
    #[test]
    fn hashes_address_then_big_endian_value() {
        assert_eq!(
            address_u32_key(address!("0x0000000000000000000000000000000000000000"), 0x0),
            b256!("0x827b659bbda2a0bdecce2c91b8b68462545758f3eba2dbefef18e0daf84f5ccd")
        );
        assert_eq!(
            address_u32_key(address!("0x1111111111111111111111111111111111111111"), 0x7),
            b256!("0x3f7497b246f36c1cd9efeedad970d582eae1a180b81a42c7c9c75f8932875751")
        );
        assert_eq!(
            address_u32_key(
                address!("0x00000000000000000000000000000000000000ff"),
                0xffffffff
            ),
            b256!("0x406d97aa3bb3260395de74823b5cf46ed995dd4156c8fbaf850312a036333ff3")
        );
        assert_eq!(
            address_u32_key(
                address!("0x2222222222222222222222222222222222222222"),
                0x1020304
            ),
            b256!("0x6069c73e3819c5753f210841163a186c9d884293a46805bd1b77ec69b5aa3f83")
        );
    }
}
