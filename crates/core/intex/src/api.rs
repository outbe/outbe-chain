//! Public Rust-to-Rust API for the Intex module.
//!
//! This is the only surface IntexFactory uses to read and write the registry.
//! The lifecycle gates mirror the Origin `IntexNFT1155` state machine
//! (`markQualified` / `markCalled`). There is no precompile dispatch for writes
//! and access is Rust-to-Rust only, so no trusted-caller checks are needed.
//!
//! The legacy registry remains a thin ledger whose record validation is the
//! `issued_at` existence sentinel. Once a certified contributor generation is
//! installed, its series identity is closed to legacy contributor writes so
//! the two storage models cannot become competing authorities. Independent
//! series creation remains valid because activation may precede auction
//! completion. Other business validation (caps, defaults, zero economic
//! parameters) belongs to the caller (IntexFactory).

use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::errors::IntexError;
use crate::schema::{
    CertifiedContributorGenerationProjection, CreateSeriesParams, DistProgress, IntexContract,
    IntexState, SeriesRecord,
};
use crate::series_id::SeriesId;

/// Immutable contributor target state consumed by OCOMP JobIntent assembly.
///
/// An absent legacy series starts at version 0 and an existing legacy series
/// at version 1. A certified root installation advances that exact version
/// once; the version is active output authority, not reservation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcompContributorTargetProjection {
    pub series_id: u32,
    pub expected_series_version: u64,
    pub contributor_count: u32,
    pub contributor_total: U256,
}

/// Create a new Intex series record. Always born in `Issued`. Rejects a
/// duplicate `series_id` (via the record-level create) and a zero `issued_at`
/// (which would make the record read back as non-existent).
pub fn create_series(storage: &StorageHandle<'_>, params: CreateSeriesParams) -> Result<()> {
    if params.issued_at == 0 {
        return Err(IntexError::ZeroIssuedAt.into());
    }

    let mut registry = IntexContract::new(storage.clone());
    let record = SeriesRecord {
        series_id: params.series_id,
        // u128 -> U256 widening is always lossless (see schema docs).
        promis_load_minor: U256::from(params.promis_load_minor),
        entry_price_minor: params.entry_price_minor,
        floor_price_minor: params.floor_price_minor,
        issued_intex_count: params.issued_intex_count,
        call_window: params.call_trigger.call_window,
        call_threshold: params.call_trigger.call_threshold,
        call_price_minor: params.call_price_minor,
        state: IntexState::Issued as u8,
        issued_at: params.issued_at,
        called_at: 0,
        call_notice_period: params.call_trigger.call_notice_period,
        issuance_currency: params.issuance_currency,
        reference_currency: params.reference_currency,
        worldwide_day: params.worldwide_day,
    };
    registry.create_series_record(&record)
}

/// `Issued -> Qualified`. Mirrors `markQualified`.
pub fn mark_qualified(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    if record.lifecycle_state()? != IntexState::Issued {
        return Err(IntexError::InvalidState {
            expected: IntexState::Issued as u8,
            actual: record.state,
        }
        .into());
    }
    record.state = IntexState::Qualified as u8;
    registry.update_series_record(&record)
}

/// `Issued | Qualified -> Called`. Mirrors `markCalled`. `called_at` is the
/// block timestamp supplied by the caller (deterministic; no wall clock here).
/// `Called` is terminal for these transitions.
pub fn mark_called(storage: &StorageHandle<'_>, series_id: SeriesId, called_at: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    let mut record = registry.load_series(series_id)?;
    let state = record.lifecycle_state()?;
    if state != IntexState::Issued && state != IntexState::Qualified {
        return Err(IntexError::InvalidState {
            expected: IntexState::Qualified as u8,
            actual: record.state,
        }
        .into());
    }
    record.state = IntexState::Called as u8;
    record.called_at = called_at;
    registry.update_series_record(&record)
}

/// Read a series record; errors if the series does not exist.
pub fn read_series(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<SeriesRecord> {
    IntexContract::new(storage.clone()).load_series(series_id)
}

/// Read a series record; `None` if the series does not exist.
pub fn get_series(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
) -> Result<Option<SeriesRecord>> {
    IntexContract::new(storage.clone()).get_series(series_id)
}

/// Whether a series exists.
pub fn series_exists(storage: &StorageHandle<'_>, series_id: SeriesId) -> Result<bool> {
    IntexContract::new(storage.clone()).series_exists(series_id)
}

/// Reads the exact create-only contributor target state for one worldwide day.
pub fn ocomp_contributor_target_projection(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
) -> Result<OcompContributorTargetProjection> {
    let registry = IntexContract::new(storage.clone());
    let exists = registry.day_has_series(worldwide_day)?;
    let legacy_count = registry.read_contributor_count(worldwide_day)?;
    let legacy_total = registry.read_contributor_total(worldwide_day)?;
    if (legacy_count == 0) != legacy_total.is_zero() {
        return Err(outbe_primitives::error::PrecompileError::Fatal(
            "Intex legacy contributor aggregate is malformed".into(),
        ));
    }
    let certified = registry.ocomp_certified_contributor_generation(worldwide_day)?;
    if certified.is_some() && (legacy_count != 0 || !legacy_total.is_zero()) {
        return Err(outbe_primitives::error::PrecompileError::Fatal(
            "Intex series has conflicting legacy and certified contributor authority".into(),
        ));
    }
    if !exists && certified.is_none() && (legacy_count != 0 || !legacy_total.is_zero()) {
        return Err(outbe_primitives::error::PrecompileError::Fatal(
            "Intex absent series has residual contributor state".into(),
        ));
    }

    let base_version = u64::from(exists);
    let (expected_series_version, contributor_count, contributor_total) = match certified {
        Some(certified) => {
            if certified.series_version < base_version {
                return Err(outbe_primitives::error::PrecompileError::Fatal(
                    "Intex certified contributor version precedes the series ledger".into(),
                ));
            }
            (
                certified.series_version,
                certified.contributor_count,
                certified.eligible_nominal_total,
            )
        }
        None => (base_version, legacy_count, legacy_total),
    };
    Ok(OcompContributorTargetProjection {
        series_id: worldwide_day,
        expected_series_version,
        contributor_count,
        contributor_total,
    })
}

/// Reads the constant-size certified contributor proof authority for a series.
pub fn certified_contributor_generation(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
) -> Result<Option<CertifiedContributorGenerationProjection>> {
    IntexContract::new(storage.clone()).ocomp_certified_contributor_generation(worldwide_day)
}

/// Number of series ever created (for dense enumeration).
pub fn total_series(storage: &StorageHandle<'_>) -> Result<u64> {
    IntexContract::new(storage.clone()).read_total_series()
}

/// `series_id` at a dense enumeration index.
pub fn series_id_at(storage: &StorageHandle<'_>, index: u64) -> Result<SeriesId> {
    IntexContract::new(storage.clone()).read_series_id_at(index)
}

// -------------------------------------------------------------------------
// Creator-reward: contributors + paginated distribution
// -------------------------------------------------------------------------

/// Record the (pre-deduplicated) legacy contributor list for a series. Called
/// once per series by legacy Lysis, before the tributes are burned. Each entry
/// is `(tribute owner, Σ nominal_amount_minor)`. A certified generation closes
/// this path for that exact series identity.
pub fn record_contributors(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    contributors: &[(Address, U256)],
) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    if registry
        .ocomp_certified_contributor_generation(worldwide_day)?
        .is_some()
    {
        return Err(PrecompileError::Revert(
            "legacy contributors cannot replace certified contributor authority".into(),
        ));
    }
    registry.write_contributors(worldwide_day, contributors)
}

/// Number of contributors recorded for a series (0 if none).
pub fn contributor_count(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<u32> {
    IntexContract::new(storage.clone()).read_contributor_count(worldwide_day)
}

/// Σ of all contributor nominals for a series (the proportionality denominator).
pub fn contributor_total(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<U256> {
    IntexContract::new(storage.clone()).read_contributor_total(worldwide_day)
}

/// `(owner, nominal)` of the contributor at a dense index.
pub fn contributor_at(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    index: u32,
) -> Result<(Address, U256)> {
    IntexContract::new(storage.clone()).read_contributor_at(worldwide_day, index)
}

/// Full contributor list for a series.
pub fn read_contributors(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
) -> Result<Vec<(Address, U256)>> {
    let registry = IntexContract::new(storage.clone());
    let count = registry.read_contributor_count(worldwide_day)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        out.push(registry.read_contributor_at(worldwide_day, i)?);
    }
    Ok(out)
}

/// Open a paginated distribution for a series: create the progress record
/// (cursor 0, nothing paid yet) and enroll the series in the active set the
/// begin-block hook drains.
pub fn start_distribution(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    amount: U256,
    total_nominal: U256,
) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    registry.create_dist_progress(&DistProgress {
        worldwide_day,
        amount,
        total_nominal,
        paid_so_far: U256::ZERO,
        cursor: 0,
        active: 1,
    })?;
    registry.push_active_dist(worldwide_day)
}

/// In-flight distribution progress for a series; `None` when none is open.
pub fn get_progress(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
) -> Result<Option<DistProgress>> {
    IntexContract::new(storage.clone()).get_dist_progress(worldwide_day)
}

/// Persist an updated progress record (advanced cursor / paid total).
pub fn save_progress(storage: &StorageHandle<'_>, progress: &DistProgress) -> Result<()> {
    IntexContract::new(storage.clone()).update_dist_progress(progress)
}

/// Finish a distribution: drop the progress record, the active-set entry, and
/// the (now spent) contributor list.
pub fn clear_distribution(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    registry.delete_dist_progress(worldwide_day)?;
    registry.remove_active_dist(worldwide_day)?;
    registry.clear_contributors(worldwide_day)
}

/// End one distribution round without touching the contributor map: drop the
/// progress record and the active-set entry only. Used by the multi-chain
/// proceeds flow, which decides separately whether to retain the map for a
/// later top-up ([`finalize_proceeds`]) or clear it.
pub fn finish_distribution_round(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    registry.delete_dist_progress(worldwide_day)?;
    registry.remove_active_dist(worldwide_day)
}

/// Number of in-flight distributions (for the begin-block drain).
pub fn active_dist_count(storage: &StorageHandle<'_>) -> Result<u32> {
    IntexContract::new(storage.clone()).read_active_dist_count()
}

/// `worldwide_day` of the active distribution at a dense index.
pub fn active_dist_at(storage: &StorageHandle<'_>, index: u32) -> Result<u32> {
    IntexContract::new(storage.clone()).read_active_dist_at(index)
}

// -------------------------------------------------------------------------
// Creator-reward: multi-chain proceeds fan-in aggregation
// -------------------------------------------------------------------------

/// Arm proceeds fan-in for a series: mark the winning chains expected (deduped),
/// set the fan-in `deadline`, and enroll the series in the awaiting-proceeds set
/// the begin-block sweep watches. Called once at issuance.
pub fn arm_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    winning_chains: &[u32],
    deadline: u64,
) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    let mut expected: u32 = 0;
    for &chain in winning_chains {
        let key = IntexContract::proceeds_chain_key(worldwide_day, chain);
        if registry.proceeds_expected.read(&key)? == 0 {
            registry.proceeds_expected.write(&key, 1u8)?;
            expected += 1;
        }
    }
    registry
        .proceeds_expected_count
        .write(&worldwide_day, expected)?;
    registry.proceeds_deadline.write(&worldwide_day, deadline)?;
    registry.push_awaiting_proceeds(worldwide_day)
}

/// Credit a chain's proceeds into the series pot; counts the chain toward the
/// fan-in once (dedup), so a chain routing its proceeds in parts is idempotent
/// for completeness while still summing every amount.
pub fn credit_proceeds(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    let registry = IntexContract::new(storage.clone());
    let pot = registry.proceeds_pot.read(&worldwide_day)?;
    registry.proceeds_pot.write(&worldwide_day, pot + amount)?;

    let expected_key = IntexContract::proceeds_chain_key(worldwide_day, src_chain_id);
    let arrived_key = expected_key;
    if registry.proceeds_expected.read(&expected_key)? != 0
        && registry.proceeds_arrived.read(&arrived_key)? == 0
    {
        registry.proceeds_arrived.write(&arrived_key, 1u8)?;
        let arrived = registry.proceeds_arrived_count.read(&worldwide_day)?;
        registry
            .proceeds_arrived_count
            .write(&worldwide_day, arrived + 1)?;
    }
    Ok(())
}

/// Every expected winning chain has routed its proceeds.
pub fn proceeds_ready(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<bool> {
    let registry = IntexContract::new(storage.clone());
    let expected = registry.proceeds_expected_count.read(&worldwide_day)?;
    Ok(expected > 0 && registry.proceeds_arrived_count.read(&worldwide_day)? == expected)
}

/// Fan-in deadline for a series (0 if never armed).
pub fn proceeds_deadline(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<u64> {
    IntexContract::new(storage.clone())
        .proceeds_deadline
        .read(&worldwide_day)
}

/// Read and clear the accumulated pot (handed to a distribution round).
pub fn take_proceeds_pot(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<U256> {
    let registry = IntexContract::new(storage.clone());
    let pot = registry.proceeds_pot.read(&worldwide_day)?;
    registry.proceeds_pot.write(&worldwide_day, U256::ZERO)?;
    Ok(pot)
}

/// Mark whether the current distribution round should finalize the series on
/// completion (all expected chains in) or retain its map for a later top-up.
pub fn set_proceeds_finalize_on_done(
    storage: &StorageHandle<'_>,
    worldwide_day: u32,
    finalize: bool,
) -> Result<()> {
    IntexContract::new(storage.clone())
        .proceeds_finalize_on_done
        .write(&worldwide_day, u8::from(finalize))
}

/// Whether the in-flight round finalizes the series on completion.
pub fn proceeds_finalize_on_done(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<bool> {
    Ok(IntexContract::new(storage.clone())
        .proceeds_finalize_on_done
        .read(&worldwide_day)?
        != 0)
}

/// Finalize proceeds aggregation for a series: clear the pot/deadline/counters,
/// drop it from the awaiting set, and clear the (now spent) contributor map.
/// The per-(series, chain) flags are left as harmless dead entries — a series id
/// (the worldwide day) never recurs.
pub fn finalize_proceeds(storage: &StorageHandle<'_>, worldwide_day: u32) -> Result<()> {
    let mut registry = IntexContract::new(storage.clone());
    registry.proceeds_pot.clear(&worldwide_day)?;
    registry.proceeds_deadline.clear(&worldwide_day)?;
    registry.proceeds_expected_count.clear(&worldwide_day)?;
    registry.proceeds_arrived_count.clear(&worldwide_day)?;
    registry.proceeds_finalize_on_done.clear(&worldwide_day)?;
    registry.remove_awaiting_proceeds(worldwide_day)?;
    registry.clear_contributors(worldwide_day)
}

/// Number of series awaiting proceeds fan-in (for the begin-block deadline sweep).
pub fn awaiting_proceeds_count(storage: &StorageHandle<'_>) -> Result<u32> {
    IntexContract::new(storage.clone())
        .awaiting_proceeds_count
        .read()
}

/// `worldwide_day` awaiting proceeds at a dense index.
pub fn awaiting_proceeds_at(storage: &StorageHandle<'_>, index: u32) -> Result<u32> {
    IntexContract::new(storage.clone())
        .awaiting_proceeds_at
        .read(&index)
}
