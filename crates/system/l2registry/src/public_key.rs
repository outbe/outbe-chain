//! EIP-2537 G2 public keys: x.c0 || x.c1 || y.c0 || y.c1, each padded to 64 bytes.
//! Compressed keys are an internal storage representation, never a wire input.

use commonware_codec::{DecodeExt, Encode};
use commonware_cryptography::bls12381::primitives::group::G2;
use outbe_primitives::error::Result;

use crate::{errors::L2RegistryError, BLS_PUBLIC_KEY_LEN};

/// Decodes a canonical, nonidentity EIP-2537 G2 public key, including subgroup validation.
pub fn decode(public_key: &[u8]) -> Result<G2> {
    if public_key.len() != BLS_PUBLIC_KEY_LEN {
        return Err(L2RegistryError::InvalidPublicKeyLength {
            length: public_key.len(),
        }
        .into());
    }
    // blst's uncompressed encoding orders components as x.c1, x.c0, y.c1, y.c0.
    let mut uncompressed = [0; 192];
    for (destination, component) in [1, 0, 3, 2].into_iter().enumerate() {
        let field = &public_key[component * 64..(component + 1) * 64];
        // EIP coordinates cannot carry blst's compression/infinity/sign flags.
        if field[..16] != [0; 16] || field[16] & 0xe0 != 0 {
            return Err(L2RegistryError::InvalidPublicKey.into());
        }
        uncompressed[destination * 48..(destination + 1) * 48].copy_from_slice(&field[16..]);
    }
    let key = blst::min_sig::PublicKey::deserialize(&uncompressed)
        .map_err(|_| L2RegistryError::InvalidPublicKey)?;
    // The common decoder rejects infinity and points outside the G2 subgroup.
    G2::decode(key.compress().as_slice()).map_err(|_| L2RegistryError::InvalidPublicKey.into())
}

/// Encodes a nonidentity G2 public key in the registry's EIP-2537 wire format.
pub fn encode(public_key: &G2) -> Result<[u8; BLS_PUBLIC_KEY_LEN]> {
    expand(public_key.encode().as_ref())
}

/// Expands existing compact storage without changing its layout or accepting
/// compressed keys at any public registration/update boundary.
pub(crate) fn expand(compressed: &[u8]) -> Result<[u8; BLS_PUBLIC_KEY_LEN]> {
    let key = blst::min_sig::PublicKey::key_validate(compressed)
        .map_err(|_| L2RegistryError::InvalidPublicKey)?;
    let serialized = key.serialize();
    let mut encoded = [0; BLS_PUBLIC_KEY_LEN];
    for (destination, component) in [1, 0, 3, 2].into_iter().enumerate() {
        encoded[destination * 64 + 16..(destination + 1) * 64]
            .copy_from_slice(&serialized[component * 48..(component + 1) * 48]);
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    // H2 generator coordinates from https://eips.ethereum.org/EIPS/eip-2537.
    // An independent vector prevents a matching encoder/decoder permutation bug.
    const GENERATOR: [u8; 256] = alloy_primitives::hex!(
        "00000000000000000000000000000000
         024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb8
         00000000000000000000000000000000
         13e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e
         00000000000000000000000000000000
         0ce5d527727d6e118cc9cdc6da2e351aadfd9baa8cbdd3a76d429a695160d12c923ac9cc3baca289e193548608b82801
         00000000000000000000000000000000
         0606c4a02ea734cc32acd2b02bc28b99cb3e287e85a763af267492ab572e99ab3f370d275cec1da1aaa9075ff05f79be"
    );

    #[test]
    fn eip_generator_roundtrip_rejects_compressed_wire_encoding() {
        let point = decode(&GENERATOR).unwrap();
        assert_eq!(encode(&point).unwrap(), GENERATOR);
        assert_eq!(
            point.encode().as_ref(),
            alloy_primitives::hex!(
                "93e02b6052719f607dacd3a088274f65596bd0d09920b61ab5da61bbdc7f5049334cf11213945d57e5ac7d055d042b7e
                 024aa2b2f08f0a91260805272dc51051c6e47ad4fa403b02b4510b647ae3d1770bac0326a805bbefd48056c8c121bdb8"
            )
        );
        assert!(decode(&point.encode()).is_err());
    }

    #[test]
    fn rejects_invalid_eip_points() {
        assert!(decode(&GENERATOR[..255]).is_err());
        assert!(decode(&[0; 256]).is_err());
        let mut invalid = GENERATOR;
        invalid[0] = 1; // Nonzero field padding.
        assert!(decode(&invalid).is_err());
        invalid = GENERATOR;
        invalid[80] |= 0x20; // Serialization flags are not field bits.
        assert!(decode(&invalid).is_err());
        invalid = GENERATOR;
        invalid[16..64].fill(0x1f); // Exceeds the field modulus.
        assert!(decode(&invalid).is_err());
        invalid = GENERATOR;
        invalid[255] ^= 1; // Off-curve point.
        assert!(decode(&invalid).is_err());

        // Valid curve point whose order is not the prime subgroup order.
        let mut compressed = [0; 96];
        compressed[0] = 0x80;
        compressed[95] = 2;
        let point = blst::min_sig::PublicKey::uncompress(&compressed).unwrap();
        assert_eq!(
            point.validate(),
            Err(blst::BLST_ERROR::BLST_POINT_NOT_IN_GROUP)
        );
        let serialized = point.serialize();
        let mut outside_subgroup = [0; 256];
        for (destination, component) in [1, 0, 3, 2].into_iter().enumerate() {
            outside_subgroup[destination * 64 + 16..(destination + 1) * 64]
                .copy_from_slice(&serialized[component * 48..(component + 1) * 48]);
        }
        assert!(decode(&outside_subgroup).is_err());
    }
}
