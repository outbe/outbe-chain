//! Bond custody, transitions, claims, and trusted Credis accounting.
use crate::{
    errors::CcaError,
    precompile::ICca,
    schema::{CcaContract, CcaRecord},
    state::decode_state,
};
use alloy_primitives::{uint, Address, U256};
use outbe_primitives::{
    addresses::CCA_ADDRESS, error::Result, storage::StorageHandle, time::WorldwideDay,
};

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
            state: ICca::State::Unknown as u8,
            bonded_amount: U256::ZERO,
            unbond_unlock_after: 0,
            exists: true,
            reward_amount: U256::ZERO,
        });
        decode_state(record.state)?;
        if record.state == ICca::State::Deregistering as u8 {
            return Err(CcaError::UnbondPending.into());
        }
        record.bonded_amount = record
            .bonded_amount
            .checked_add(amount)
            .ok_or(CcaError::Arithmetic)?;
        record.state = if record.bonded_amount >= BOND_REQUIREMENT {
            ICca::State::Active
        } else {
            ICca::State::Unknown
        } as u8;
        contract.save(&record)?;
        contract.emit(ICca::Bonded {
            cca: caller,
            amount,
            selfBond: record.bonded_amount,
            state: decode_state(record.state)?,
        })
    })
}

pub fn unbond(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(caller)?;
        let state = decode_state(record.state)?;
        if record.state == ICca::State::Deregistering as u8 {
            return Err(CcaError::UnbondPending.into());
        }
        if record.bonded_amount.is_zero() {
            return Err(CcaError::InvalidAmount.into());
        }
        if !matches!(state, ICca::State::Active | ICca::State::Unknown) {
            return Err(CcaError::InvalidState(record.state).into());
        }
        record.unbond_unlock_after = now(&storage)?
            .checked_add(UNBOND_COOLDOWN_SECONDS)
            .ok_or(CcaError::Arithmetic)?;
        record.state = ICca::State::Deregistering as u8;
        contract.save(&record)?;
        contract.emit(ICca::UnbondRequested {
            cca: caller,
            amount: record.bonded_amount,
            completeTime: record.unbond_unlock_after,
        })
    })
}

pub fn claim_unbonded(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(caller)?;
        if record.bonded_amount.is_zero() {
            return Err(CcaError::NoUnbond.into());
        }
        if record.state != ICca::State::Deregistering as u8 {
            return Err(CcaError::InvalidState(record.state).into());
        }
        if now(&storage)? < record.unbond_unlock_after {
            return Err(CcaError::Cooldown.into());
        }
        let amount = record.bonded_amount;
        record.bonded_amount = U256::ZERO;
        record.unbond_unlock_after = 0;
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
        let amount = record.reward_amount;
        if amount.is_zero() {
            return Err(CcaError::NoRewards.into());
        }
        record.reward_amount = U256::ZERO;
        contract.save(&record)?;
        storage.transfer_balance(CCA_ADDRESS, caller, amount)?;
        contract.emit(ICca::RewardsClaimed {
            cca: caller,
            amount,
        })
    })
}

/// Trusted Rust entrypoint; called once by Credis with its sealed origination day.
pub fn position_opened(
    storage: &StorageHandle<'_>,
    cca: Address,
    day: WorldwideDay,
    gratis: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let contract = CcaContract::new(storage.clone());
        if contract.load(cca)?.state != ICca::State::Active as u8 {
            return Err(CcaError::NotActive.into());
        }
        let key = CcaContract::reward_weight_key(cca, day);
        let deficit = contract.reward_deficits.read(&key)?;
        let offset = gratis.min(deficit);
        let weight = contract
            .reward_weights
            .read(&key)?
            .checked_add(gratis - offset)
            .ok_or(CcaError::Arithmetic)?;
        // offset <= both gratis and deficit, so both subtractions are exact.
        contract.reward_deficits.write(&key, deficit - offset)?;
        contract.reward_weights.write(&key, weight)
    })
}

/// Subtract burned collateral from the void-day bucket, even after exit.
/// Excess burns offset later same-day openings; prior days and accrued rewards stay unchanged.
pub fn position_voided(
    storage: &StorageHandle<'_>,
    cca: Address,
    day: WorldwideDay,
    gratis_burned: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let contract = CcaContract::new(storage.clone());
        contract.load(cca)?;
        let key = CcaContract::reward_weight_key(cca, day);
        let weight = contract.reward_weights.read(&key)?;
        let offset = gratis_burned.min(weight);
        let deficit = contract
            .reward_deficits
            .read(&key)?
            .checked_add(gratis_burned - offset)
            .ok_or(CcaError::Arithmetic)?;
        // offset <= both gratis_burned and weight; retain any excess as a deficit.
        let weight = weight - offset;
        contract.reward_deficits.write(&key, deficit)?;
        contract.reward_weights.write(&key, weight)
    })
}
