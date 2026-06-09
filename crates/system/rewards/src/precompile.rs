use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate, reject_value, view};
use outbe_primitives::error::Result;

use crate::schema::Rewards;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IRewards.sol"
);

/// Dispatches an ABI-encoded call to the Rewards precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(data, IRewards::IRewardsCalls::abi_decode, |call| {
        let mut rewards = Rewards::new(storage);
        use IRewards::IRewardsCalls::*;
        match call {
            claimRewards(c) => mutate(c, caller, |sender, _c| rewards.claim_rewards(sender)),
            pendingRewards(c) => view(c, |c| rewards.pending_rewards_of(c.validator)),
        }
    })
}
