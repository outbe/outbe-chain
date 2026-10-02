//! Settlement statement bound into a PayNote spend proof.
//!
//! The circuit treats `context` as an opaque non-zero field. This module is the
//! only place that defines what that field means: the canonical BN254 element of
//! `keccak256(domain || target || units || snapshot)`.

use alloy_primitives::{keccak256, B256, U256};
use ark_ff::Zero;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_protocol::codec::{self, field_from_be_bytes};

use crate::errors::PayNoteError;

/// Which product a spend proof is allowed to settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SettlementDomain {
    Nod = 1,
    Gem = 2,
    Intex = 3,
}

/// Left-align a 14-byte Intex series id in a 32-byte word.
pub fn intex_series_target(series_id: &[u8; 14]) -> B256 {
    let mut word = [0u8; 32];
    word[..14].copy_from_slice(series_id);
    B256::from(word)
}

fn domain_byte(domain: SettlementDomain) -> u8 {
    match domain {
        SettlementDomain::Nod => 1,
        SettlementDomain::Gem => 2,
        SettlementDomain::Intex => 3,
    }
}

/// Canonical context word for one settlement.
///
/// `target` is the nod or gem id as 32 big-endian bytes, or an Intex series id
/// in the first 14 bytes. `units` is 1 for a nod or gem and the selected amount
/// for an Intex series. `snapshot` is the quote's VWAP id, or zero on the
/// reference rail.
///
/// A reduction that lands on zero is a prove-time error: the circuit rejects it,
/// so a caller must not attempt to bind that statement.
pub fn settlement_context(
    domain: SettlementDomain,
    target: B256,
    units: U256,
    snapshot: U256,
) -> Result<B256> {
    let mut preimage = [0u8; 97];
    preimage[0] = domain_byte(domain);
    preimage[1..33].copy_from_slice(target.as_slice());
    preimage[33..65].copy_from_slice(&units.to_be_bytes::<32>());
    preimage[65..97].copy_from_slice(&snapshot.to_be_bytes::<32>());
    let reduced = field_from_be_bytes(keccak256(preimage).as_slice());
    if reduced.is_zero() {
        return Err(
            PayNoteError::InvalidInput("settlement context collapses to zero".into()).into(),
        );
    }
    codec::field_to_b256(&reduced).map_err(|error| PrecompileError::Fatal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(byte: u8) -> B256 {
        B256::from(U256::from(byte))
    }

    #[test]
    fn context_is_keccak_reduced_into_a_canonical_field() {
        let nod =
            settlement_context(SettlementDomain::Nod, word(0x11), U256::ONE, U256::ZERO).unwrap();
        assert_eq!(
            nod,
            B256::from(alloy_primitives::hex!(
                "0bf6bb50d4732574255428c9866829f136b25b93d46ac49c0206ce5f94297742"
            ))
        );

        let snapshot = U256::from_be_bytes(alloy_primitives::hex!(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
        ));
        let gem =
            settlement_context(SettlementDomain::Gem, word(0x22), U256::ONE, snapshot).unwrap();
        assert_eq!(
            gem,
            B256::from(alloy_primitives::hex!(
                "09dc3b5852005bdecf313136d31d59cf27e5c6a69b8b6ef2ab345a1d75c219d5"
            ))
        );

        let series = *b"20260212-TRY-U";
        let intex = settlement_context(
            SettlementDomain::Intex,
            intex_series_target(&series),
            U256::from(7u64),
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            intex,
            B256::from(alloy_primitives::hex!(
                "1f0ea2f024789bee0075321892de18a3fcf110e88565746a6fed9823cb465cbc"
            ))
        );
        assert_ne!(nod, gem);
        assert_ne!(nod, intex);
    }

    #[test]
    fn domain_byte_and_series_padding_change_the_word() {
        let target = intex_series_target(b"20260212-TRY-U");
        assert_eq!(&target.as_slice()[..14], b"20260212-TRY-U");
        assert!(target.as_slice()[14..].iter().all(|byte| *byte == 0));
        let nod = settlement_context(SettlementDomain::Nod, target, U256::from(7u64), U256::ZERO)
            .unwrap();
        let intex = settlement_context(
            SettlementDomain::Intex,
            target,
            U256::from(7u64),
            U256::ZERO,
        )
        .unwrap();
        assert_ne!(nod, intex);
    }
}
