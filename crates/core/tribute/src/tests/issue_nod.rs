use super::{in_process_ready, sample_tribute, set_day, set_owner};
use crate::TributeContract;
use alloy_primitives::{address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::storage::{CycleLifecyclePurpose, MutationPurposeTag};
use outbe_system_common::addresses::TRIBUTE_ADDRESS;
use outbe_system_common::metadosis_bindings::metadosis_process_ready_binding;
use outbe_system_common::time::EmissionDate;

// These fixtures share a chain that predates their December 2024 WorldwideDays.
const GENESIS_TIMESTAMP: u64 = 1_733_011_200; // 2024-12-01 00:00 UTC

/// The id `issueNod` gives the Tribute's Nod: the owner in the series of its price day.
fn nod_id_of(tc: &TributeContract<'_>, tribute: &crate::schema::TributeData) -> U256 {
    let series = outbe_core_common::series_id::SeriesId::for_pair(
        tc.lysis_emission_day(tribute.worldwide_day)
            .unwrap()
            .unwrap()
            .value(),
        tribute.issuance_currency,
        tribute.reference_currency,
    )
    .unwrap();
    outbe_core_common::nod_rules::derive_nod_id(series, tribute.owner)
}

/// Seals the Lysis of `day` for its linked emission day, or the price day of `completed_at`.
fn seal_lysis(
    tc: &mut TributeContract<'_>,
    day: outbe_system_common::time::WorldwideDay,
    metadosis_limit: U256,
    completed_at: u64,
) -> outbe_primitives::error::Result<(U256, U256)> {
    let storage = tc.storage_handle();
    let emission_day = match tc.lysis_emission_day(day)? {
        Some(emission_day) => emission_day,
        None => EmissionDate::new(outbe_nod::openings::entry_price_day(completed_at).unwrap()),
    };
    let priced = outbe_oracle::api::priced_reference_currencies_of_day(
        storage.clone(),
        emission_day.value(),
    )?
    .into_iter()
    .filter(|&(_, vwap)| outbe_nod::pricing::is_issuable_entry(vwap))
    .collect();
    let outcome = tc.compute_and_record_metadosis_fractions(
        day,
        emission_day,
        &priced,
        metadosis_limit,
        completed_at,
    )?;
    Ok((outcome.nominal_minor, outcome.allocated_minor))
}

/// Prices `iso` on `utc_day` and on the day before it.
fn seed_entry_price(storage: &StorageHandle<'_>, iso: u16, utc_day: u32, vwap: U256) {
    let pair = outbe_oracle::api::AddressPair::new_coen_to(iso);
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    let index = match oracle.pair_index_of(pair).unwrap() {
        0 => outbe_oracle::api::register_pair(storage.clone(), pair).unwrap(),
        index => index,
    };
    for day in [
        outbe_system_common::time::previous_date_key(utc_day).unwrap(),
        utc_day,
    ] {
        oracle.record_utc_day_vwap(day, index, vwap).unwrap();
    }
}

fn test_day_league_fraction(
    tc: &TributeContract<'_>,
    day: outbe_system_common::time::WorldwideDay,
    league: u16,
) -> u64 {
    let Some(emission_day) = tc.lysis_emission_day(day).unwrap() else {
        return 0;
    };
    let Some(record) = tc.day_fractions_record(emission_day).unwrap() else {
        return 0;
    };
    let fractions = crate::enclave_client::read_day_fractions(&record).unwrap();
    fractions.get(&league).copied().unwrap_or(0)
}

mod cleanup;
mod issuance;
mod multi_league;

/// Runs the Nod daily pass at `timestamp` through the UTC day before it, one day per block
/// from `height`, as the Cycle does.
fn run_nod_daily_pass(storage: &StorageHandle<'_>, mut height: u64, timestamp: u64) {
    use outbe_system_common::time::{previous_date_key, timestamp_to_date_key};

    let closed = previous_date_key(timestamp_to_date_key(timestamp)).unwrap();
    let oracle = outbe_oracle::schema::OracleContract::new(storage.clone());
    if oracle.utc_day_vwap_last_finalized.read().unwrap() < closed {
        oracle.utc_day_vwap_last_finalized.write(closed).unwrap();
    }
    let nod = outbe_nod::NodContract::new(storage.clone());
    if nod.call_latest_day.read().unwrap() == 0 {
        // This fixture starts from an established parent with only the newest day pending.
        nod.call_latest_day
            .write(previous_date_key(closed).unwrap())
            .unwrap();
    }
    loop {
        let ctx = outbe_primitives::block::BlockRuntimeContext::new(
            outbe_primitives::block::BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: height,
                timestamp,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: GENESIS_TIMESTAMP,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            storage.clone(),
        );
        outbe_nodfactory::called::process_call_pass(&ctx).unwrap();
        let selected = outbe_system_common::pass_budget::BlockAllowance::unpack(
            nod.daily_budget.read().unwrap(),
        )
        .unwrap()
        .map_or(0, |allowance| allowance.day);
        let next = nod.call_sweep_day.read().unwrap();
        if next == 0 || next == selected {
            assert!(
                next == 0 || selected == nod.call_latest_day.read().unwrap(),
                "the pass stopped on day {selected} before the last closed day"
            );
            break;
        }
        height += 1;
    }
}
