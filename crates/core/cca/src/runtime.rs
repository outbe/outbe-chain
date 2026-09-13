//! Bond custody, transitions, claims, and trusted Credis accounting.
use crate::{
    errors::CcaError,
    precompile::ICca,
    schema::{CcaContract, CcaRecord},
    state::decode_state,
};
use alloy_primitives::{uint, Address, U256};
use outbe_primitives::{addresses::CCA_ADDRESS, error::Result, storage::StorageHandle};

/// One billion whole COEN, in 18-decimal native atomic units.
pub const BOND_REQUIREMENT: U256 = uint!(1_000_000_000_000_000_000_000_000_000_U256);
pub const UNBOND_COOLDOWN_SECONDS: u64 = 128 * 86_400;

fn now(storage: &StorageHandle<'_>) -> Result<u64> {
    // Execution timestamps must fit Unix seconds in u64; reject rather than truncate.
    storage
        .timestamp()?
        .try_into()
        .map_err(|_| CcaError::Arithmetic.into())
}

/// The payable EVM boundary has already credited `amount` to CCA_ADDRESS.
pub fn bond(storage: StorageHandle<'_>, caller: Address, amount: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        if caller.is_zero() {
            return Err(CcaError::ZeroAddress.into());
        }
        if amount.is_zero() {
            return Err(CcaError::InvalidAmount.into());
        }
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.records.get(caller)?.unwrap_or(CcaRecord {
            cca: caller,
            state: ICca::State::Suspended as u8,
            self_bond: U256::ZERO,
            unbond_amount: U256::ZERO,
            unbond_complete_time: 0,
            reward_weight: U256::ZERO,
            claimable_rewards: U256::ZERO,
        });
        decode_state(record.state)?;
        if !record.unbond_amount.is_zero() {
            return Err(CcaError::UnbondPending.into());
        }
        record.self_bond = record
            .self_bond
            .checked_add(amount)
            .ok_or(CcaError::Arithmetic)?;
        record.state = if record.self_bond >= BOND_REQUIREMENT {
            ICca::State::Active
        } else {
            ICca::State::Suspended
        } as u8;
        contract.save(&record)?;
        contract.emit(ICca::Bonded {
            cca: caller,
            amount,
            selfBond: record.self_bond,
            state: decode_state(record.state)?,
        })
    })
}

pub fn unbond(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(caller)?;
        let state = decode_state(record.state)?;
        if !record.unbond_amount.is_zero() {
            return Err(CcaError::UnbondPending.into());
        }
        if record.self_bond.is_zero() {
            return Err(CcaError::InvalidAmount.into());
        }
        if !matches!(state, ICca::State::Active | ICca::State::Suspended) {
            return Err(CcaError::InvalidState(record.state).into());
        }
        record.unbond_complete_time = now(&storage)?
            .checked_add(UNBOND_COOLDOWN_SECONDS)
            .ok_or(CcaError::Arithmetic)?;
        record.unbond_amount = record.self_bond;
        record.self_bond = U256::ZERO;
        record.state = ICca::State::Suspended as u8;
        contract.save(&record)?;
        contract.emit(ICca::UnbondRequested {
            cca: caller,
            amount: record.unbond_amount,
            completeTime: record.unbond_complete_time,
        })
    })
}

pub fn claim_unbonded(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(caller)?;
        if record.unbond_amount.is_zero() {
            return Err(CcaError::NoUnbond.into());
        }
        if record.state != ICca::State::Suspended as u8 {
            return Err(CcaError::InvalidState(record.state).into());
        }
        if now(&storage)? < record.unbond_complete_time {
            return Err(CcaError::Cooldown.into());
        }
        let amount = record.unbond_amount;
        record.unbond_amount = U256::ZERO;
        record.unbond_complete_time = 0;
        record.state = ICca::State::Deregistered as u8;
        contract.save(&record)?;
        storage.transfer_balance(CCA_ADDRESS, caller, amount)?;
        contract.emit(ICca::UnbondClaimed {
            cca: caller,
            amount,
        })
    })
}

pub fn claim_rewards(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(caller)?;
        let amount = record.claimable_rewards;
        if amount.is_zero() {
            return Err(CcaError::NoRewards.into());
        }
        record.claimable_rewards = U256::ZERO;
        contract.save(&record)?;
        storage.transfer_balance(CCA_ADDRESS, caller, amount)?;
        contract.emit(ICca::RewardsClaimed {
            cca: caller,
            amount,
        })
    })
}

/// Trusted Rust entrypoint; called once by Credis's opening transition.
pub fn position_opened(storage: &StorageHandle<'_>, cca: Address, gratis: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(cca)?;
        if record.state != ICca::State::Active as u8 {
            return Err(CcaError::NotActive.into());
        }
        record.reward_weight = record
            .reward_weight
            .checked_add(gratis)
            .ok_or(CcaError::Arithmetic)?;
        contract.save(&record)?;
        contract.emit(ICca::RewardWeightChanged {
            cca,
            weight: record.reward_weight,
        })
    })
}

/// Subtract only the remaining collateral burned, including for exited agents.
pub fn position_voided(
    storage: &StorageHandle<'_>,
    cca: Address,
    gratis_burned: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(cca)?;
        record.reward_weight = record
            .reward_weight
            .checked_sub(gratis_burned)
            .ok_or(CcaError::Arithmetic)?;
        contract.save(&record)?;
        contract.emit(ICca::RewardWeightChanged {
            cca,
            weight: record.reward_weight,
        })
    })
}
