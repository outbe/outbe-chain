use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::sol;
use outbe_primitives::dispatch::reject_value;
use outbe_primitives::error::Result;

use crate::errors::RewardsError;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IRewards.sol"
);

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

/// Dispatches an ABI-encoded call to the Rewards precompile.
///
/// The Rewards precompile exposes **no** callable external methods. The Rewards
/// daily emission accrues separately in AgentReward, which owns reward claims.
/// Per-block fees settle internally via the `LateFinalizeCredits` begin-zone
/// phase. The contract's state is accessed only in-process through the
/// [`crate::schema::Rewards`] facade (api / lifecycle / hooks), never through
/// this inbound ABI. `REWARDS_ADDRESS` stays a preserved system account holding
/// the fee escrow, so any external call deterministically reverts.
pub fn dispatch(
    _storage: outbe_primitives::storage::StorageHandle,
    _data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    Err(RewardsError::NoCallableMethods.into())
}
