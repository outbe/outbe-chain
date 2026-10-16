//! Sponsored-transaction fixtures for tests in other crates.

use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::{Address, U256};
use outbe_ocomp_protocol::transaction_call::TransactionCallFields;
use outbe_primitives::addresses::AGENT_REWARD_ADDRESS;

use crate::ZeroFeeTransaction;

/// A well-formed sponsored call from `signer` to AgentReward.
pub fn sponsored_envelope(signer: Address, input: &[u8]) -> ZeroFeeTransaction<'_> {
    ZeroFeeTransaction {
        signer,
        call: TransactionCallFields {
            to: Some(AGENT_REWARD_ADDRESS),
            value: U256::ZERO,
            input,
            gas_limit: 100_000,
            max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
            max_priority_fee_per_gas: Some(0),
        },
    }
}
