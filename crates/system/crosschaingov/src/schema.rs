use alloy_primitives::{B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;

/// First storage slot of this module. `outbe-hyperlanecontroller` keeps its
/// state in the slots below it, in the same account (`0x...EE14`).
pub const FIRST_SLOT: u64 = 10;

/// EVM storage layout for the CrosschainGov precompile.
///
/// Storage slots (the Hyperlane controller uses slots 0-9):
///  10: approved_payload - mapping(proposal id => keccak256 of the approved
///      crosschain action payload). Zero = none or already applied
///  11: approved_at - mapping(proposal id => block of the approval)
#[storage_schema]
#[contract(addr = CROSSCHAIN_GOV_ADDRESS)]
pub struct CrosschainGov {
    #[slot(10)]
    pub approved_payload: outbe_primitives::storage::dsl::Map<U256, B256>,

    pub approved_at: outbe_primitives::storage::dsl::Map<U256, u64>,
}
