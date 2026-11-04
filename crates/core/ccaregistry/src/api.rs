//! Shared registry queries and trusted Credis-accounting entrypoints.
pub use crate::precompile::ICcaRegistry;
pub use crate::runtime::{credis_forfeited, credis_issued};
use crate::{
    constants::{DAY_CLEANUP_ENTRIES_PER_BLOCK, DAY_CLEANUP_HEADERS_PER_BLOCK},
    errors::{CcaError, CcaFatalError},
    schema::{address_day_key, day_index_key, CcaContract},
    state::validate_state,
};
use alloy_primitives::{Address, U256};
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};

pub fn cca_state(storage: &StorageHandle<'_>, cca: Address) -> Result<ICcaRegistry::State> {
    Ok(CcaContract::new(storage.clone()).load(cca)?.state)
}

pub fn is_active(storage: &StorageHandle<'_>, cca: Address) -> Result<bool> {
    match CcaContract::new(storage.clone()).records.get(cca)? {
        Some(record) => Ok(validate_state(record.state)? == ICcaRegistry::State::Active),
        None => Ok(false),
    }
}

/// Reverts unless `cca` is recorded in `Active` standing.
pub fn require_active_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<()> {
    let record = CcaContract::new(storage.clone()).load(cca)?;
    if record.state == ICcaRegistry::State::Active {
        Ok(())
    } else {
        Err(CcaError::CcaNotActive {
            cca,
            state: record.state,
        }
        .into())
    }
}

/// Raw GRATIS in one UTC day bucket (YYYYMMDD). Distribution is the only step that normalizes it.
pub fn reward_weight(storage: &StorageHandle<'_>, cca: Address, day: u32) -> Result<U256> {
    CcaContract::new(storage.clone())
        .gratis_minor_per_utc_day
        .read(&address_day_key(cca, day))
}

pub fn get_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<ICcaRegistry::Cca> {
    let contract = CcaContract::new(storage.clone());
    let record = contract.load(cca)?;
    Ok(ICcaRegistry::Cca {
        cca: record.cca,
        name: record.name,
        state: record.state,
        bondedMinor: record.bonded_minor,
        unbondUnlocksAfter: record.unbond_unlocks_after,
    })
}

/// Native COEN bonded across all records and not yet claimed back.
pub fn total_bonded(storage: &StorageHandle<'_>) -> Result<U256> {
    CcaContract::new(storage.clone()).total_bonded_minor.read()
}

/// Refuses a day after the Rewards `last_sealed_day`. Rewards seals the days in
/// order, so its `last_sealed_day` is the only sealed-day state.
fn require_sealed(storage: &StorageHandle<'_>, day: u32) -> Result<()> {
    if day > outbe_rewards::api::get_last_sealed_day(storage)? {
        return Err(CcaError::RewardDayNotSealed.into());
    }
    Ok(())
}

/// Frozen entry count (including zero weights) and net GRATIS denominator.
pub fn reward_day_inputs(storage: &StorageHandle<'_>, day: u32) -> Result<(u32, U256)> {
    let contract = CcaContract::new(storage.clone());
    require_sealed(storage, day)?;
    Ok((
        contract.reward_day_count.read(&day)?,
        contract.reward_day_total.read(&day)?,
    ))
}

/// Read one historical entry without consulting current standing.
pub fn reward_day_entry(
    storage: &StorageHandle<'_>,
    day: u32,
    index: u32,
) -> Result<(Address, U256)> {
    let contract = CcaContract::new(storage.clone());
    require_sealed(storage, day)?;
    if index >= contract.reward_day_count.read(&day)? {
        return Err(CcaFatalError::RewardDayIndexCorrupt.into());
    }
    let address = contract
        .reward_day_addresses
        .read(&day_index_key(day, index))?;
    if address.is_zero()
        || !contract
            .reward_day_members
            .read(&address_day_key(address, day))?
    {
        return Err(CcaFatalError::RewardDayIndexCorrupt.into());
    }
    Ok((address, reward_weight(storage, address, day)?))
}

/// Trusted caller schedules only after the monetary CCA job is terminal or abandoned.
pub fn enqueue_day_cleanup(ctx: &BlockRuntimeContext<'_>, day: u32) -> Result<()> {
    ctx.with_checkpoint(|| {
        let contract = CcaContract::new(ctx.storage.clone());
        require_sealed(&ctx.storage, day)?;
        if contract.day_cleanup_scheduled.read(&day)? {
            return Ok(());
        }
        let tail = contract.day_cleanup_tail.read()?;
        let next = tail.checked_add(1).ok_or(CcaError::Arithmetic)?;
        if contract.day_cleanup_head.read()? > tail {
            return Err(CcaFatalError::RewardDayIndexCorrupt.into());
        }
        contract.day_cleanup_queue.write(&tail, day)?;
        contract.day_cleanup_tail.write(next)?;
        contract.day_cleanup_scheduled.write(&day, true)
    })
}

/// Delete at most 256 entries and inspect at most four queue headers per block.
/// The checkpoint includes the height marker, so failed slices remain retryable.
pub fn cleanup(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    ctx.with_checkpoint(|| cleanup_inner(ctx))
}

fn cleanup_inner(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
    let storage = &ctx.storage;
    let contract = CcaContract::new(storage.clone());
    if contract.has_cleanup_processed_block.read()?
        && contract.cleanup_processed_block.read()? == ctx.block.block_number
    {
        return Ok(());
    }
    let mut head = contract.day_cleanup_head.read()?;
    let tail = contract.day_cleanup_tail.read()?;
    if head > tail {
        return Err(CcaFatalError::RewardDayIndexCorrupt.into());
    }
    let mut budget = DAY_CLEANUP_ENTRIES_PER_BLOCK;
    for _ in 0..DAY_CLEANUP_HEADERS_PER_BLOCK {
        if head == tail {
            break;
        }
        let day = contract.day_cleanup_queue.read(&head)?;
        if !contract.day_cleanup_scheduled.read(&day)? {
            return Err(CcaFatalError::RewardDayIndexCorrupt.into());
        }
        require_sealed(storage, day)?;
        let count = contract.reward_day_count.read(&day)?;
        let mut cursor = contract.day_cleanup_cursor.read(&day)?;
        if cursor > count {
            return Err(CcaFatalError::RewardDayIndexCorrupt.into());
        }
        while cursor < count && budget > 0 {
            let (address, _) = reward_day_entry(storage, day, cursor)?;
            let key = address_day_key(address, day);
            contract.gratis_minor_per_utc_day.clear(&key)?;
            contract.gratis_deficit_minor_per_utc_day.clear(&key)?;
            contract.reward_day_members.clear(&key)?;
            contract
                .reward_day_addresses
                .clear(&day_index_key(day, cursor))?;
            // cursor < count <= u32::MAX, so this increment is bounded.
            cursor = cursor.checked_add(1).ok_or(CcaError::Arithmetic)?;
            budget -= 1;
        }
        if cursor < count {
            contract.day_cleanup_cursor.write(&day, cursor)?;
            break;
        }
        contract.reward_day_count.clear(&day)?;
        contract.reward_day_total.clear(&day)?;
        contract.day_cleanup_cursor.clear(&day)?;
        contract.day_cleanup_queue.clear(&head)?;
        head = head.checked_add(1).ok_or(CcaError::Arithmetic)?;
    }
    contract.day_cleanup_head.write(head)?;
    contract
        .cleanup_processed_block
        .write(ctx.block.block_number)?;
    contract.has_cleanup_processed_block.write(true)
}
