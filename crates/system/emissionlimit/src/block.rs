//! Terminal-Metadosis dispatch helper.
//!
//! After (Phase 4 of the Cycle epic), EmissionLimit is no longer wired into the per-block
//! lifecycle. The previous `EmissionLimitLifecycle::begin_block` / `run_begin_block` /
//! `dispatch_block_emission` triple was removed. The day orchestration runs in the new
//! Cycle module. That module:
//!
//! - reads the closed-form `day_emission_limit`
//! - calls [`crate::allocation::allocate_emission`] with the 5-sink active table
//! - hands the reward pools to `outbe_agentreward::distribute_daily`
//! - forwards the Metadosis terminal portion through [`dispatch_terminal_remainder_at`] below.
//!
//! This file is intentionally tiny. It owns the purpose-bound terminal dispatch calls that
//! Cycle, late fee settlement and reward accrual use. Keeping them distinct prevents a
//! non-daily residue from becoming a daily base-limit producer.

use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::errors::EmissionLimitError;

/// Sends the daily Cycle terminal allocation to the Metadosis sink, anchored at
/// `timestamp`.
///
/// The Cycle day handler dispatches the previous UTC day's terminal Metadosis
/// amount. The sink must use that previous-day timestamp so that worldwide-day
/// accounting lands in the right bucket. This holds regardless of when the call
/// physically runs.
///
/// Returns `Revert` if the Metadosis sink reports any unused amount.
/// The terminal sink must consume the full amount.
pub fn dispatch_terminal_remainder_at(
    ctx: &BlockRuntimeContext,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    let mut terminal_block = ctx.block.clone();
    terminal_block.timestamp = timestamp;
    let terminal_ctx = BlockRuntimeContext::new(terminal_block, ctx.storage.clone());
    let unused = outbe_metadosis::commands::apply_cycle_day_limit(&terminal_ctx, amount)?;
    if !unused.is_zero() {
        return Err(EmissionLimitError::TerminalSinkReturnedUnusedAmount.into());
    }
    Ok(())
}

/// Recycles a late residue, from fee settlement or from a finished WAA or SRA
/// reward pool, into terminal Metadosis headroom.
///
/// This function is deliberately distinct from [`dispatch_terminal_remainder_at`].
/// Only the daily Cycle allocation forms a base day limit. Late residues
/// accumulate in the Promis Limit for the next unformed limit.
pub fn dispatch_late_settlement_residue_at(
    ctx: &BlockRuntimeContext,
    amount: U256,
    timestamp: u64,
) -> Result<()> {
    if amount.is_zero() {
        return Ok(());
    }

    let mut terminal_block = ctx.block.clone();
    terminal_block.timestamp = timestamp;
    let terminal_ctx = BlockRuntimeContext::new(terminal_block, ctx.storage.clone());
    let unused = outbe_metadosis::commands::apply_late_settlement_headroom(&terminal_ctx, amount)?;
    if !unused.is_zero() {
        return Err(EmissionLimitError::LateSettlementSinkReturnedUnusedAmount.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;
    use outbe_primitives::addresses::PROMIS_LIMIT_ADDRESS;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::MetadosisMutationPurposeTag;
    use outbe_primitives::time::date_key_to_utc_timestamp;

    const CHAIN_ID: u64 = 1;
    const CTX_DAY: u32 = 20_260_803;
    const SINK_DAY: u32 = 20_260_802;

    fn timestamp(day: u32) -> u64 {
        date_key_to_utc_timestamp(day) + 3_600
    }

    fn promis_balance(storage: &outbe_primitives::storage::StorageHandle<'_>) -> U256 {
        storage.sload(PROMIS_LIMIT_ADDRESS, U256::ZERO).unwrap()
    }

    fn emission_limit(
        storage: &outbe_primitives::storage::StorageHandle<'_>,
        day: u32,
    ) -> Option<U256> {
        outbe_metadosis::api::emission_day(storage.clone(), day)
            .unwrap()
            .map(|day| day.emission_limit_minor)
    }

    #[test]
    fn zero_residue_is_a_noop_and_zero_terminal_uses_the_supplied_day() {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let events_before = provider.events.clone();
        provider.enter(|storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::new(7, timestamp(CTX_DAY), CHAIN_ID, Address::ZERO, Vec::new()),
                storage.clone(),
            );
            dispatch_late_settlement_residue_at(&ctx, U256::ZERO, timestamp(SINK_DAY)).unwrap();
            assert!(promis_balance(&storage).is_zero());
            assert_eq!(emission_limit(&storage, SINK_DAY), None);
            assert_eq!(emission_limit(&storage, CTX_DAY), None);
        });
        assert_eq!(provider.events, events_before);

        provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
        provider.enter(|storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::new(7, timestamp(CTX_DAY), CHAIN_ID, Address::ZERO, Vec::new()),
                storage.clone(),
            );
            dispatch_terminal_remainder_at(&ctx, U256::ZERO, timestamp(SINK_DAY)).unwrap();
            assert_eq!(emission_limit(&storage, SINK_DAY), Some(U256::ZERO));
            assert_eq!(emission_limit(&storage, CTX_DAY), None);
            assert!(promis_balance(&storage).is_zero());
        });
    }

    #[test]
    fn terminal_dispatch_without_a_lease_writes_nothing() {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        let events_before = provider.events.clone();
        provider.enter(|storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::new(7, timestamp(CTX_DAY), CHAIN_ID, Address::ZERO, Vec::new()),
                storage.clone(),
            );
            let error =
                dispatch_terminal_remainder_at(&ctx, U256::from(10u64), timestamp(SINK_DAY))
                    .unwrap_err();
            assert_eq!(
                error.to_string(),
                "revert: test provider has no matching Metadosis mutation lease"
            );
            assert_eq!(emission_limit(&storage, SINK_DAY), None);
            assert_eq!(emission_limit(&storage, CTX_DAY), None);
            assert!(promis_balance(&storage).is_zero());
        });
        assert_eq!(provider.events, events_before);
    }
}
