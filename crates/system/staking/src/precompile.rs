use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, metadata, mutate_void, mutate_void_payable, reject_value, view,
};
use outbe_primitives::error::Result;

use crate::contract::Staking;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IStaking.sol"
);

/// Dispatches an ABI-encoded call to the Staking precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    dispatch_call(data, IStaking::IStakingCalls::abi_decode, |call| {
        let mut staking = Staking::new(storage);
        use IStaking::IStakingCalls::*;
        match call {
            stake(c) => mutate_void_payable(c, caller, value, |sender, c, val| {
                if val != c.amount {
                    return Err(outbe_primitives::error::PrecompileError::Revert(
                        "msg.value must equal stake amount".into(),
                    ));
                }
                staking.stake(sender, c.validatorAddress, c.amount)
            }),
            unstake(c) => {
                reject_value(&value)?;
                mutate_void(c, caller, |sender, c| staking.unstake(sender, c.amount))
            }
            claimUnbonded(c) => {
                reject_value(&value)?;
                mutate_void(c, caller, |sender, _c| staking.claim_unbonded(sender))
            }
            unjailValidator(c) => {
                reject_value(&value)?;
                mutate_void(c, caller, |sender, _c| staking.unjail_validator(sender))
            }
            getStake(c) => view(c, |c| staking.get_stake(c.validator)),
            getTotalStaked(_) => {
                metadata::<IStaking::getTotalStakedCall>(|| staking.get_total_staked())
            }
        }
    })
}
