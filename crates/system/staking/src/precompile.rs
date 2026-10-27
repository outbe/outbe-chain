use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, metadata, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
    PayableCallContext,
};
use outbe_primitives::error::Result;

use crate::contract::Staking;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] =
    &[IStaking::stakeCall::SELECTOR, IStaking::topupCall::SELECTOR];

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
    // Staking is a payable route, so the boundary credits value to this
    // address. Every selector the module has not published refuses it here.
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(data, IStaking::IStakingCalls::abi_decode, |call| {
        let mut staking = Staking::new(storage.clone());
        use IStaking::IStakingCalls::*;
        match call {
            stake(c) => mutate_void_payable(
                &storage,
                c,
                PayableCallContext {
                    selectors: PAYABLE_SELECTORS,
                    sender: caller,
                    value,
                },
                |sender, c, val| {
                    if val != c.amount {
                        return Err(crate::errors::StakingError::MsgValueMismatch.into());
                    }
                    let (pubkey, sig) =
                        outbe_validatorset::logic::decode_registration_key_and_proof(
                            &c.consensusPubkey,
                            &c.blsRegistrationSignature,
                        )?;
                    staking.stake(
                        sender,
                        c.validatorAddress,
                        c.amount,
                        (&pubkey, c.radicleNodeId, sig),
                        (c.p2pVersion, &c.p2pEncoded),
                    )
                },
            ),
            topup(c) => mutate_void_payable(
                &storage,
                c,
                PayableCallContext {
                    selectors: PAYABLE_SELECTORS,
                    sender: caller,
                    value,
                },
                |sender, c, val| {
                    if val != c.amount {
                        return Err(crate::errors::StakingError::MsgValueMismatch.into());
                    }
                    staking.topup(sender, c.validatorAddress, c.amount)
                },
            ),
            unstake(c) => mutate_void(&storage, c, caller, |sender, c| {
                staking.unstake(sender, c.amount)
            }),
            claimUnbonded(c) => mutate_void(&storage, c, caller, |sender, _c| {
                staking.claim_unbonded(sender)
            }),
            unjailValidator(c) => mutate_void(&storage, c, caller, |sender, _c| {
                staking.unjail_validator(sender)
            }),
            getStake(c) => view(c, |c| staking.get_stake(c.validator)),
            getTotalStaked(_) => {
                metadata::<IStaking::getTotalStakedCall>(|| staking.get_total_staked())
            }
            getMinStake(_) => {
                metadata::<IStaking::getMinStakeCall>(|| staking.config_min_stake.read())
            }
            getUnbondingPeriod(_) => metadata::<IStaking::getUnbondingPeriodCall>(|| {
                staking.config_unbonding_period.read()
            }),
            getMaxStakePercent(_) => metadata::<IStaking::getMaxStakePercentCall>(|| {
                staking.config_max_stake_percent.read()
            }),
            getSlashedWithdrawalDelay(_) => {
                metadata::<IStaking::getSlashedWithdrawalDelayCall>(|| {
                    staking.slashed_withdrawal_delay()
                })
            }
        }
    })
}
