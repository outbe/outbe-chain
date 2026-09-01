//! Tribute-owned FullProof context binding.
//!
//! The generic circuit protocol treats `binding_hash` as opaque. Tribute keeps
//! its established application formula here so the enclave, prover fixtures,
//! and verifier expectations cannot drift.

use outbe_protocol::primitive::hash::FieldHasher;
use outbe_protocol::{codec::field_from_be_bytes, OutbeV1, Suite};

type Field = <OutbeV1 as Suite>::Field;

const TRIBUTE_BINDING_DOMAIN_V1: u64 = 1;
/// `Poseidon5([domain, sender, commitment_id_lo128, commitment_id_hi128, chain_id])`.
///
/// The two 128-bit commitment-ID limbs are the established Tribute context
/// encoding. They are intentionally unrelated to the `[120, 120, 16]` amount
/// limbs used by Emit and PayNote.
pub fn tribute_binding(
    sender: &[u8; 20],
    commitment_id: &[u8; 32],
    chain_id: u64,
) -> Result<Field, outbe_protocol::error::Error> {
    let domain = Field::from(TRIBUTE_BINDING_DOMAIN_V1);
    let sender = field_from_be_bytes::<Field>(sender);
    let high = u128::from_be_bytes(commitment_id[..16].try_into().expect("16-byte slice"));
    let low = u128::from_be_bytes(commitment_id[16..].try_into().expect("16-byte slice"));
    <<OutbeV1 as Suite>::Hash as FieldHasher<Field>>::hash(&[
        domain,
        sender,
        Field::from(low),
        Field::from(high),
        Field::from(chain_id),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_protocol::Codec;

    #[test]
    fn tribute_binding_fixed_vector() {
        let binding = tribute_binding(&[1; 20], &[2; 32], 19_280_501).unwrap();
        assert_eq!(
            OutbeV1::field_to_be_bytes(&binding),
            alloy_primitives::hex!(
                "1483126ac1c6965d35549a1e091acd4ab4015680c21b61f9383069707ca39988"
            )
        );
    }
}
