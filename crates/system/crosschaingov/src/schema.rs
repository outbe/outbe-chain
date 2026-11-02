use alloy_primitives::{B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;

/// EVM storage layout of `crosschaingov` at the owner address.
///
/// Slots 0-31 belong to `hyperlanecontroller` at the same address.
///
/// Storage slots:
///  32: approved_payload - mapping(proposal id => keccak256 of the approved
///      action payload). Zero = none or already applied
///  33: approved_at - mapping(proposal id => block of the approval)
#[storage_schema]
#[contract(addr = CROSSCHAIN_GOV_ADDRESS)]
pub struct CrosschainGovContract {
    #[slot(32)]
    pub approved_payload: outbe_primitives::storage::dsl::Map<U256, B256>,

    pub approved_at: outbe_primitives::storage::dsl::Map<U256, u64>,
}
