//! Bounded local garbage collection, separate from terminal economic transitions.

use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_system_common::time::EmissionDate;

use crate::{
    constants::{CLEANUP_HEADER_UNITS, CLEANUP_MAX_HEADERS, CLEANUP_WORK_UNITS},
    AuctionStage, DesisContract, DesisError,
};

/// Process one atomic cleanup page. Errors, including sticky budget errors, propagate.
pub fn process_cleanup(storage: StorageHandle<'_>) -> Result<()> {
    storage.with_checkpoint(|| cleanup_page(&storage))
}

fn cleanup_page(storage: &StorageHandle<'_>) -> Result<()> {
    let contract = storage.contract::<DesisContract>();
    let mut units = CLEANUP_WORK_UNITS;
    for _ in 0..CLEANUP_MAX_HEADERS {
        if units < CLEANUP_HEADER_UNITS {
            break;
        }
        let Some(day) = queued_day(&contract)? else {
            break;
        };
        units -= CLEANUP_HEADER_UNITS;
        let chains = contract.intake_chain_count.read(&day)?;
        if chains == 0 {
            finish_day(&contract, day)?;
        } else {
            units -= cleanup_chain(&contract, day, chains, units)?;
        }
    }
    Ok(())
}

fn queued_day(contract: &DesisContract<'_>) -> Result<Option<EmissionDate>> {
    let raw_day = contract.cleanup_head.read()?;
    if raw_day == 0 {
        if contract.cleanup_tail.read()? != 0 {
            return Err(DesisError::InvalidCleanupState.into());
        }
        return Ok(None);
    }
    let day = EmissionDate::new(raw_day);
    if contract.cleanup_pending.read(&day)? != 1
        || !matches!(
            contract.read_stage(day)?,
            AuctionStage::Completed | AuctionStage::Cancelled | AuctionStage::Failed
        )
    {
        return Err(DesisError::InvalidCleanupState.into());
    }
    Ok(Some(day))
}

fn finish_day(contract: &DesisContract<'_>, day: EmissionDate) -> Result<()> {
    let next = contract.cleanup_next.read(&day)?;
    let tail = contract.cleanup_tail.read()?;
    if (next == 0) != (tail == day.value()) {
        return Err(DesisError::InvalidCleanupState.into());
    }
    contract.cleanup_head.write(next)?;
    if next == 0 {
        contract.cleanup_tail.write(0)?;
    }
    contract.cleanup_next.clear(&day)?;
    contract.cleanup_pending.write(&day, 2)
}

fn cleanup_chain(
    contract: &DesisContract<'_>,
    day: EmissionDate,
    chains: u32,
    units: u32,
) -> Result<u32> {
    let row = DesisContract::chain_key(day, chains - 1);
    let chain = contract.intake_chain_at.read(&row)?;
    let key = DesisContract::chain_key(day, chain);
    if contract.intake_chain_member.read(&key)? != 1 {
        return Err(DesisError::InvalidCleanupState.into());
    }
    let count = contract.chain_bid_count.read(&key)?;
    let remove = count.min(units);
    let remaining = count - remove;
    // Each bid is exactly two storage clears (10k gross); persist its cursor once per page.
    for index in remaining..count {
        let bid = DesisContract::bid_key(day, chain, index);
        contract.bid_bidder.clear(&bid)?;
        contract.bid_packed.clear(&bid)?;
    }
    if remaining != 0 {
        contract.chain_bid_count.write(&key, remaining)?;
    } else {
        // Nine metadata writes, covered by the conservative 80k header allowance.
        contract.chain_bid_count.clear(&key)?;
        contract.chain_total_batches.clear(&key)?;
        contract.chain_arrived_mask.clear(&key)?;
        contract.chain_done.clear(&key)?;
        contract.chain_done_batches.clear(&key)?;
        contract.chain_done_bids.clear(&key)?;
        contract.intake_chain_member.clear(&key)?;
        contract.intake_chain_at.clear(&row)?;
        contract.intake_chain_count.write(&day, chains - 1)?;
    }
    Ok(remove)
}

#[cfg(test)]
#[path = "tests/cleanup.rs"]
mod tests;
