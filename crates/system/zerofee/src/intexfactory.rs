use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;

use crate::envelope::ZeroFeeEnvelope;

/// Exact ABI envelope cap for a full batch: selector, four head words, then a
/// 256-leaf array of four-word structs and a 24-hash proof.
pub const MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES: usize =
    4 + 4 * 32 + (32 + 256 * 4 * 32) + (32 + 24 * 32);

/// Only explicit storage ops are metered inside precompiles (reads 100,
/// writes 5000). Transfers and events are journal ops outside metering, so a
/// full batch executes in tens of thousands of gas.
pub const MAX_ZERO_FEE_CONTRIBUTOR_BATCH_GAS_LIMIT: u64 =
    21_000 + 16 * MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES as u64 + 500_000;

/// Public txpool compatibility floor. The executor waives the actual debit
/// only after stateful validator authorization.
pub const MIN_ZERO_FEE_CONTRIBUTOR_BATCH_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;

/// Stateless envelope of a zero-fee contributor batch payment.
///
/// Decoding is bounded by the caps, and it is the only way to reject a head
/// that points outside the payload.
pub(crate) const CONTRIBUTOR_BATCH_ENVELOPE: ZeroFeeEnvelope = ZeroFeeEnvelope {
    target: INTEX_FACTORY_ADDRESS,
    min_max_fee_per_gas: MIN_ZERO_FEE_CONTRIBUTOR_BATCH_MAX_FEE_PER_GAS,
    max_calldata_bytes: MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES,
    max_gas_limit: MAX_ZERO_FEE_CONTRIBUTOR_BATCH_GAS_LIMIT,
    malformed_reason: "payContributorBatch decode failed",
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{ZeroFeeHookId, ZeroFeePolicyError, ZeroFeeTransaction};
    use crate::test_support::hook_tx;
    use alloy_primitives::{address, Address, B256, U256};
    use alloy_sol_types::SolCall;
    use outbe_intexfactory::precompile::IIntexFactory;
    use outbe_ocomp_protocol::transaction_call::TransactionCallFields;

    const VALIDATOR: Address = address!("0x1111111111111111111111111111111111111111");

    fn calldata(leaves: usize, proof: usize) -> Vec<u8> {
        IIntexFactory::payContributorBatchCall {
            worldwideDay: 20_260_725,
            startIndex: 0,
            leaves: (0..leaves)
                .map(|i| IIntexFactory::ContributorLeaf {
                    owner: Address::repeat_byte(u8::try_from(i % 251).unwrap()),
                    sourceTributeId: U256::from_be_bytes(B256::repeat_byte(7).0),
                    nominal: U256::from(1_u64),
                })
                .collect(),
            proof: vec![B256::repeat_byte(9); proof],
        }
        .abi_encode()
    }

    fn tx(input: &[u8]) -> ZeroFeeTransaction<'_> {
        let base = hook_tx(VALIDATOR, INTEX_FACTORY_ADDRESS, input);
        ZeroFeeTransaction {
            call: TransactionCallFields {
                gas_limit: MAX_ZERO_FEE_CONTRIBUTOR_BATCH_GAS_LIMIT,
                max_fee_per_gas: MIN_ZERO_FEE_CONTRIBUTOR_BATCH_MAX_FEE_PER_GAS,
                ..base.call
            },
            ..base
        }
    }

    #[test]
    fn a_full_batch_fits_the_envelope() {
        let input = calldata(256, 24);
        assert!(
            input.len() <= MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES,
            "full batch is {} bytes, cap is {}",
            input.len(),
            MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES
        );
        let candidate = crate::registry().classify(&tx(&input)).unwrap().unwrap();
        assert_eq!(
            candidate.hook,
            ZeroFeeHookId::IntexFactoryPayContributorBatch
        );
    }

    #[test]
    fn only_the_exact_zero_fee_envelope_is_claimed() {
        let input = calldata(1, 1);

        let mut foreign_target = tx(&input);
        foreign_target.call.to = Some(Address::ZERO);
        assert!(crate::registry()
            .classify(&foreign_target)
            .unwrap()
            .is_none());

        let other_selector = IIntexFactory::distributeCall {
            worldwideDay: 1,
            srcChainId: 1,
        }
        .abi_encode();
        assert!(crate::registry()
            .classify(&tx(&other_selector))
            .unwrap()
            .is_none());

        // A tip-paying sender keeps the normal fee market.
        let mut tipped = tx(&input);
        tipped.call.max_priority_fee_per_gas = Some(1);
        assert!(crate::registry().classify(&tipped).unwrap().is_none());
    }

    #[test]
    fn value_oversize_and_malformed_calldata_are_rejected() {
        let input = calldata(1, 1);

        let mut funded = tx(&input);
        funded.call.value = U256::from(1_u64);
        assert_eq!(
            crate::registry().classify(&funded).unwrap_err(),
            ZeroFeePolicyError::NonZeroValue
        );

        let mut greedy = tx(&input);
        greedy.call.gas_limit = MAX_ZERO_FEE_CONTRIBUTOR_BATCH_GAS_LIMIT + 1;
        assert!(matches!(
            crate::registry().classify(&greedy).unwrap_err(),
            ZeroFeePolicyError::GasLimitTooHigh { .. }
        ));

        let mut cheap = tx(&input);
        cheap.call.max_fee_per_gas = MIN_ZERO_FEE_CONTRIBUTOR_BATCH_MAX_FEE_PER_GAS - 1;
        assert!(matches!(
            crate::registry().classify(&cheap).unwrap_err(),
            ZeroFeePolicyError::FeeCapTooLow { .. }
        ));

        let mut oversized = calldata(256, 24);
        oversized.resize(MAX_ZERO_FEE_CONTRIBUTOR_BATCH_CALLDATA_BYTES + 1, 0);
        assert!(matches!(
            crate::registry().classify(&tx(&oversized)).unwrap_err(),
            ZeroFeePolicyError::CalldataTooLarge { .. }
        ));

        let mut truncated = input.clone();
        truncated.truncate(40);
        assert!(matches!(
            crate::registry().classify(&tx(&truncated)).unwrap_err(),
            ZeroFeePolicyError::MalformedCalldata(_)
        ));
    }
}
