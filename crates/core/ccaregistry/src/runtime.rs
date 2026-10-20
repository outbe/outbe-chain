//! Bond custody, transitions, claims, and trusted Credis accounting.
use crate::{
    constants::{BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
    errors::CcaError,
    precompile::ICcaRegistry,
    schema::{address_day_key, CcaContract, CcaRecord},
    state::validate_state,
};
use alloy_primitives::{Address, U256};
use outbe_primitives::{addresses::CCA_REGISTRY_ADDRESS, error::Result, storage::StorageHandle};

/// The payable EVM boundary has already credited `amount` to CCA_REGISTRY_ADDRESS.
pub fn bond(storage: StorageHandle<'_>, caller: Address, amount: U256, name: String) -> Result<()> {
    storage.with_checkpoint(|| {
        if caller.is_zero() {
            return Err(CcaError::ZeroAddress.into());
        }
        if amount.is_zero() {
            return Err(CcaError::InvalidAmount.into());
        }
        if name.is_empty() {
            return Err(CcaError::InvalidName.into());
        }
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.records.get(caller)?.unwrap_or(CcaRecord {
            cca: caller,
            state: ICcaRegistry::State::Bonding,
            bonded_minor: U256::ZERO,
            unbond_unlocks_after: 0,
            name: String::new(),
        });
        validate_state(record.state)?;
        if record.state == ICcaRegistry::State::Deregistering {
            return Err(CcaError::UnbondPending.into());
        }
        record.name = name;
        record.bonded_minor = record
            .bonded_minor
            .checked_add(amount)
            .ok_or(CcaError::Arithmetic)?;
        record.state = if record.bonded_minor >= BOND_REQUIREMENT {
            ICcaRegistry::State::Active
        } else {
            ICcaRegistry::State::Bonding
        };
        contract.save(&record)?;
        let total = contract
            .total_bonded_minor
            .read()?
            .checked_add(amount)
            .ok_or(CcaError::Arithmetic)?;
        set_total_bonded(&storage, &contract, total)?;
        contract.emit(ICcaRegistry::Bonded {
            cca: caller,
            amountMinor: amount,
            state: record.state,
        })
    })
}

pub fn unbond(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(caller)?;
        let state = record.state;
        if record.state == ICcaRegistry::State::Deregistering {
            return Err(CcaError::UnbondPending.into());
        }
        if record.bonded_minor.is_zero() {
            return Err(CcaError::InvalidAmount.into());
        }
        if !matches!(
            state,
            ICcaRegistry::State::Active | ICcaRegistry::State::Bonding
        ) {
            return Err(CcaError::InvalidState(record.state.into()).into());
        }
        record.unbond_unlocks_after = storage
            .now_secs()?
            .checked_add(UNBOND_COOLDOWN_SECONDS)
            .ok_or(CcaError::Arithmetic)?;
        record.state = ICcaRegistry::State::Deregistering;
        contract.save(&record)?;
        contract.emit(ICcaRegistry::UnbondRequested {
            cca: caller,
            amountMinor: record.bonded_minor,
            unbondUnlocksAfter: record.unbond_unlocks_after,
        })
    })
}

pub fn claim_unbonded(storage: StorageHandle<'_>, caller: Address) -> Result<()> {
    storage.with_checkpoint(|| {
        let mut contract = CcaContract::new(storage.clone());
        let mut record = contract.load(caller)?;
        if record.bonded_minor.is_zero() {
            return Err(CcaError::NoUnbond.into());
        }
        if record.state != ICcaRegistry::State::Deregistering {
            return Err(CcaError::InvalidState(record.state.into()).into());
        }
        if storage.now_secs()? < record.unbond_unlocks_after {
            return Err(CcaError::Cooldown.into());
        }
        let amount = record.bonded_minor;
        record.bonded_minor = U256::ZERO;
        record.unbond_unlocks_after = 0;
        record.state = ICcaRegistry::State::Deregistered;
        contract.save(&record)?;
        storage.transfer_balance(CCA_REGISTRY_ADDRESS, caller, amount)?;
        let total = contract
            .total_bonded_minor
            .read()?
            .checked_sub(amount)
            .ok_or(CcaError::Arithmetic)?;
        set_total_bonded(&storage, &contract, total)?;
        contract.emit(ICcaRegistry::UnbondClaimed {
            cca: caller,
            amountMinor: amount,
        })
    })
}

/// Records the bonded total. The registry account must hold at least that much.
fn set_total_bonded(
    storage: &StorageHandle<'_>,
    contract: &CcaContract<'_>,
    total: U256,
) -> Result<()> {
    if storage.balance(CCA_REGISTRY_ADDRESS)? < total {
        return Err(CcaError::CustodyShortfall.into());
    }
    contract.total_bonded_minor.write(total)
}

/// Trusted Rust entrypoint. Credis calls it once with the current UTC reward day key (YYYYMMDD).
pub fn credis_issued(
    storage: &StorageHandle<'_>,
    cca: Address,
    day: u32,
    gratis: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        crate::api::require_active_cca(storage, cca)?;
        let contract = CcaContract::new(storage.clone());
        let key = address_day_key(cca, day);
        let deficit = contract.gratis_deficit_minor_per_utc_day.read(&key)?;
        let offset = gratis.min(deficit);
        let weight = contract
            .gratis_minor_per_utc_day
            .read(&key)?
            .checked_add(gratis - offset)
            .ok_or(CcaError::Arithmetic)?;
        // offset <= both gratis and deficit, so both subtractions are exact.
        contract
            .gratis_deficit_minor_per_utc_day
            .write(&key, deficit - offset)?;
        contract.gratis_minor_per_utc_day.write(&key, weight)
    })
}

/// Subtract burned collateral from the forfeit-day bucket, even after exit.
/// Excess burns offset later same-day openings. Prior days and accrued rewards stay unchanged.
pub fn credis_forfeited(
    storage: &StorageHandle<'_>,
    cca: Address,
    day: u32,
    gratis_burned: U256,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let contract = CcaContract::new(storage.clone());
        contract.load(cca)?;
        let key = address_day_key(cca, day);
        let weight = contract.gratis_minor_per_utc_day.read(&key)?;
        let offset = gratis_burned.min(weight);
        let deficit = contract
            .gratis_deficit_minor_per_utc_day
            .read(&key)?
            .checked_add(gratis_burned - offset)
            .ok_or(CcaError::Arithmetic)?;
        // offset <= both gratis_burned and weight. Keep any excess as a deficit.
        let weight = weight - offset;
        contract
            .gratis_deficit_minor_per_utc_day
            .write(&key, deficit)?;
        contract.gratis_minor_per_utc_day.write(&key, weight)
    })
}
