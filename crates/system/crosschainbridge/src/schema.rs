use alloy_primitives::{B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::CROSSCHAIN_BRIDGE_ADDRESS;

/// EVM storage layout of `crosschainbridge`.
///
/// Storage slots:
///   0: approved_payload - mapping(proposal id => keccak256 of the approved
///      action payload). Zero = none or already applied
///   1: approved_at - mapping(proposal id => block of the approval)
#[storage_schema]
#[contract(addr = CROSSCHAIN_BRIDGE_ADDRESS)]
pub struct CrosschainBridgeContract {
    #[attribute(order = 0)]
    pub approved_payload: outbe_primitives::storage::dsl::Map<U256, B256>,

    #[attribute(order = 1)]
    pub approved_at: outbe_primitives::storage::dsl::Map<U256, u64>,
}
