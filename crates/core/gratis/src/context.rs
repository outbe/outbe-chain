//! Operation contexts authenticated by pledge-note proofs.
//!
//! Both circuits authenticate an opaque nonzero field; the consuming runtime
//! recomputes its meaning here. Domain bytes and encoding are protocol-stable.

use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolValue;
use outbe_primitives::{
    addresses::GRATIS_FACTORY_ADDRESS,
    error::{PrecompileError, Result},
};
use outbe_protocol::codec;

/// Which operation a pledge-note proof is allowed to authorize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PledgeDomain {
    Issue = 4,
    Unpledge = 5,
}

/// Reduce `keccak256(domain || target || amount || snapshot)` into a canonical
/// BN254 field word. Amount and snapshot use 32-byte big-endian encoding.
pub fn pledge_context(
    domain: PledgeDomain,
    target: B256,
    amount: U256,
    snapshot: U256,
) -> Result<B256> {
    let mut preimage = [0u8; 97];
    preimage[0] = domain as u8;
    preimage[1..33].copy_from_slice(target.as_slice());
    preimage[33..65].copy_from_slice(&amount.to_be_bytes::<32>());
    preimage[65..97].copy_from_slice(&snapshot.to_be_bytes::<32>());
    let reduced = codec::field_from_be_bytes(keccak256(preimage).as_slice());
    let context = codec::field_to_b256(&reduced)
        .map_err(|error| PrecompileError::Fatal(error.to_string()))?;
    if context.is_zero() {
        return Err(PrecompileError::Revert(
            "pledge: settlement context collapses to zero".into(),
        ));
    }
    Ok(context)
}

/// Wallet and runtime use the same chain/factory/destination/amount binding.
pub fn unpledge_context(chain_id: u64, destination: Address, amount: U256) -> Result<B256> {
    let target = keccak256(
        (
            U256::from(chain_id),
            GRATIS_FACTORY_ADDRESS,
            destination,
            amount,
        )
            .abi_encode(),
    );
    pledge_context(PledgeDomain::Unpledge, target, amount, U256::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_vectors_preserve_encoding_and_domain_separation() {
        // Captured from the original implementation before moving it out of Paynote.
        let target = B256::from(U256::from(17));
        let amount = U256::from(23);
        let snapshot = U256::from(42);
        let paynote_contexts = alloy_primitives::hex!(
            "14d67c55415c743ca5d48cecdb53330fa0d7fbf34d3e9aae4d1ce756be131ccc"
            "2cdd5ea0217715d2aad885a9e54128813f4c4fc23f327d537741af30a0e31ed1"
            "15dc235387de00dc226b798ce167dbb9ea337ab6e465afe21c8473b266fea962"
        );
        for (domain, expected) in [
            (
                PledgeDomain::Issue,
                alloy_primitives::hex!(
                    "09ec28a532acd094b591c8ac3ddb85b04cb7700e8906032d153f7acff14b2d22"
                ),
            ),
            (
                PledgeDomain::Unpledge,
                alloy_primitives::hex!(
                    "2dd1276e6b7fcd2d7ca85d82c286b02f9a7fa3bb2bed602d7e434cb5a0f0def3"
                ),
            ),
        ] {
            let context = pledge_context(domain, target, amount, snapshot).unwrap();
            assert_eq!(context, B256::from(expected));
            for paynote in paynote_contexts.chunks_exact(32) {
                assert_ne!(context, B256::from_slice(paynote));
            }
        }
        assert_eq!(
            unpledge_context(1, Address::repeat_byte(0x11), amount).unwrap(),
            B256::from(alloy_primitives::hex!(
                "24e4d1cc1083abbf9b141bc392923025177d9e7412475b56488fff970e6ddea8"
            )),
        );
    }

    #[test]
    fn pledge_context_binds_operation_target_amount_and_snapshot() {
        let target = B256::from(U256::from(17));
        let amount = U256::from(23);
        let snapshot = U256::from(42);
        let pledge = pledge_context(PledgeDomain::Issue, target, amount, snapshot).unwrap();
        for (domain, target, amount, snapshot) in [
            (PledgeDomain::Unpledge, target, amount, snapshot),
            (
                PledgeDomain::Issue,
                B256::from(U256::from(18)),
                amount,
                snapshot,
            ),
            (PledgeDomain::Issue, target, amount + U256::ONE, snapshot),
            (PledgeDomain::Issue, target, amount, snapshot + U256::ONE),
        ] {
            let changed = pledge_context(domain, target, amount, snapshot).unwrap();
            assert_ne!(pledge, changed);
            assert!(!changed.is_zero());
            assert!(codec::field_from_b256(&changed).is_ok());
        }
        assert!(!pledge.is_zero());
        assert!(codec::field_from_b256(&pledge).is_ok());
    }

    #[test]
    fn unpledge_context_binds_chain_destination_and_amount() {
        let destination = Address::repeat_byte(0x11);
        let amount = U256::from(23);
        let context = unpledge_context(1, destination, amount).unwrap();
        for (chain, destination, amount) in [
            (2, destination, amount),
            (1, Address::repeat_byte(0x12), amount),
            (1, destination, amount + U256::ONE),
        ] {
            assert_ne!(
                context,
                unpledge_context(chain, destination, amount).unwrap()
            );
        }
    }
}
