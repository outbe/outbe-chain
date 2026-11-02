//! Runtime helpers for the Rewards module.
//!
//! Houses the genesis-anchor lazy initialization and UTC-day helpers used
//! by `RewardsLifecycle::begin_block` and the per-block fee/participation
//! hook. These functions take a `BlockRuntimeContext` and access state
//! through `ctx.storage`, so they are thin wrappers over the schema in
//! [`crate::schema::Rewards`].
//!

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_consensus::proof::canonical_signer_set_hash;
use outbe_primitives::{
    block::BlockRuntimeContext,
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::Result,
    time::{day_number_between, timestamp_to_date_key},
};

use crate::errors::RewardsError;
use crate::schema::Rewards;

pub(crate) fn require_sealed_day(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let rewards = ctx.storage.contract::<Rewards>();
    if day == 0
        || day >= timestamp_to_date_key(ctx.block.timestamp)
        || !rewards.daily_participation_closed.read(&day)?
        || !crate::api::day_participation_complete(ctx, day)?
    {
        return Err(RewardsError::DayNotSealed.into());
    }
    Ok(())
}

pub(crate) fn allocate_day_validator_rewards(
    ctx: &BlockRuntimeContext,
    day: u32,
    validator_share: U256,
) -> Result<crate::api::ValidatorRewardAllocation> {
    use outbe_primitives::units::{checked_protocol_to_native, units_to_protocol_minor_floor};
    require_sealed_day(ctx, day)?;
    let topup = if validator_share.is_zero() {
        U256::ZERO
    } else {
        let native_budget = checked_protocol_to_native(validator_share)
            .ok_or(RewardsError::NativeValidatorBudgetOverflow)?;
        let fees = ctx
            .storage
            .contract::<Rewards>()
            .daily_fee_sum_raw
            .read(&day)?;
        units_to_protocol_minor_floor(native_budget.saturating_sub(fees))
    };
    Ok(crate::api::ValidatorRewardAllocation {
        topup,
        // Native subtraction followed by flooring cannot exceed the original share.
        fee_covered: validator_share - topup,
    })
}

pub(crate) fn enqueue_day_cleanup(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let rewards = ctx.storage.contract::<Rewards>();
    if rewards.cleanup_scheduled.read(&day)? {
        return Ok(());
    }
    require_sealed_day(ctx, day)?;
    ctx.storage.with_checkpoint(|| {
        let tail = rewards.cleanup_tail.read()?;
        let next = tail
            .checked_add(1)
            .ok_or(RewardsError::CleanupQueueOverflow)?;
        rewards.cleanup_day_at.write(&tail, day)?;
        rewards.cleanup_tail.write(next)?;
        rewards.cleanup_scheduled.write(&day, true)
    })
}

pub(crate) fn process_day_cleanup(ctx: &BlockRuntimeContext) -> Result<()> {
    ctx.storage.with_checkpoint(|| {
        let rewards = ctx.storage.contract::<Rewards>();
        if rewards.cleanup_processed_initialized.read()?
            && rewards.cleanup_processed_height.read()? == ctx.block.block_number
        {
            return Ok(());
        }
        let mut head = rewards.cleanup_head.read()?;
        let tail = rewards.cleanup_tail.read()?;
        if head > tail {
            return Err(RewardsError::CleanupQueueCorrupt.into());
        }
        let mut remaining = 256u32;
        for _ in 0..4 {
            if head == tail {
                break;
            }
            let day = rewards.cleanup_day_at.read(&head)?;
            if day == 0 || !rewards.daily_participation_closed.read(&day)? {
                return Err(RewardsError::CleanupQueueCorrupt.into());
            }
            let count = rewards.daily_voter_count.read(&day)?;
            let mut index = rewards.cleanup_next_index.read(&day)?;
            if index > count {
                return Err(RewardsError::CleanupQueueCorrupt.into());
            }
            let voters = rewards.daily_voter_at.get_nested(&day);
            let participation = rewards.daily_participation.get_nested(&day);
            while index < count && remaining > 0 {
                let voter = voters.read(&index)?;
                participation.write(&voter, 0)?;
                voters.write(&index, Address::ZERO)?;
                // index < count <= u32::MAX, so increment cannot overflow.
                index += 1;
                remaining -= 1;
            }
            if index < count {
                rewards.cleanup_next_index.write(&day, index)?;
                break;
            }
            rewards.daily_voter_count.write(&day, 0)?;
            rewards.daily_total_participation.write(&day, U256::ZERO)?;
            rewards.daily_fee_sum_raw.write(&day, U256::ZERO)?;
            rewards.daily_last_window_close.write(&day, 0)?;
            rewards.cleanup_next_index.write(&day, 0)?;
            rewards.cleanup_day_at.write(&head, 0)?;
            // head < tail <= u64::MAX, so increment cannot overflow.
            head += 1;
            rewards.cleanup_head.write(head)?;
        }
        rewards
            .cleanup_processed_height
            .write(ctx.block.block_number)?;
        rewards.cleanup_processed_initialized.write(true)
    })
}

/// Outcome of [`check_and_record_metadata_fingerprint`]. The caller MUST
/// branch on this and skip all per-block module work when
/// `IdenticalReplay` is returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataFingerprintOutcome {
    /// First time seeing this `fb_hash`. Fingerprint persisted. Caller
    /// must proceed with all module hooks.
    Fresh,
    /// Same `fb_hash` + same fingerprint already processed. Full no-op.
    /// Caller MUST skip per-block module hooks (participation, slashing,
    /// fees). They would all short-circuit anyway via per-module guards,
    /// but skipping early avoids redundant SLOAD/SSTORE work.
    IdenticalReplay,
}

// V3 fingerprint binds the V2-Certified-Parent participation
// proof identity end-to-end. Locally-observed late votes cannot alter
// the base certificate or its participation bitmap. `LateFinalizeCredits`
// accounts for authenticated late participation separately, including
// daily GEM participation. It does not relax this fingerprint. The canonical
// signer set is therefore part of the
// fingerprint via [`outbe_consensus::proof::canonical_signer_set_hash`].
//
// Fields bound by V3 (in addition to the V2 set):
// - `proof_kind` tag (Finalization / CertifiedNotarization).
// - `committee_set_hash` (already in metadata, computed by
//   `committee_set_hash_v2`).
// - `signer_set_hash = canonical_signer_set_hash(signer_bitmap)`.
// - `vrf_material_version`.
// - `vrf_group_public_key_hash`.
// - `canonical_vrf_proof_hash` - derived by the executor from the
//   verified certificate's VRF proof and threaded through
//   [`crate::runtime::check_and_record_metadata_fingerprint`] as the
//   `canonical_vrf_proof_hash` argument.
//
// Domain bump V2 -> V3 makes V3 fingerprints non-collide with any V2
// entries that pre-genesis test runs may have written.
const FINGERPRINT_DOMAIN: &[u8] = b"OUTBE_METADATA_FINGERPRINT_V3";

/// Reads the genesis UTC day from `Rewards::genesis_utc_day`, lazily
/// initializing the slot from `ctx.block.timestamp` on the first call
/// (which must be block 0). Returns the locked-in day on every
/// subsequent call.
///
/// `RewardsLifecycle::begin_block` calls this function as its very first
/// step, before any other lifecycle work. After block 0 the slot is
/// immutable. On a healthy chain the lazy init branch fires exactly once in
/// the chain's lifetime.
///
/// Tamper-resistance: a node booting with a different `genesis.json`
/// timestamp will record a different value here. Then:
/// - Subsequent `day_emission_limit` calculations (in
///   `outbe_emissionlimit::day_emission`) diverge from quorum.
/// - The post-exec state root mismatches at the first day-settle.
/// - The node leaves consensus.
///
/// Derives the genesis UTC day directly from `ctx.block.genesis_timestamp` (OIP-00343).
pub fn ensure_genesis_anchor(ctx: &BlockRuntimeContext) -> Result<u32> {
    genesis_utc_day(ctx)
}

/// Derives the genesis UTC day from `ctx.block.genesis_timestamp`. Returns `Revert`
/// if `genesis_timestamp` is zero (uninitialized).
pub fn genesis_utc_day(ctx: &BlockRuntimeContext) -> Result<u32> {
    if ctx.block.genesis_timestamp == 0 {
        return Err(RewardsError::GenesisUtcDayNotInitialized.into());
    }
    Ok(timestamp_to_date_key(ctx.block.genesis_timestamp))
}

/// Computes the integer day number of `utc_day` relative to the chain's
/// genesis day. Returns:
/// - `Ok(0)` for the genesis day itself.
/// - `Ok(n)` for `n` days after genesis.
/// - `Revert` for a `utc_day` strictly before genesis. A finalized block
///   that predates genesis is a protocol violation.
pub fn day_number_since_genesis(ctx: &BlockRuntimeContext, utc_day: u32) -> Result<u32> {
    let genesis = genesis_utc_day(ctx)?;
    day_number_between(genesis, utc_day).map_err(|e| RewardsError::from(e).into())
}

/// V3 fingerprint guard. Computes the canonical V3 metadata
/// fingerprint and then does one of these:
/// - On first sight, persists it (returns `Fresh`).
/// - On identical replay, short-circuits (returns `IdenticalReplay`).
/// - On contradictory metadata for the same `fb_hash`, returns `Revert`.
///
/// The fingerprint is the **single source of truth** for "same
/// participation proof identity" under V2 Certified-Parent Accounting.
/// Two metadata-txes for the same `fb_hash` with different proof_kind,
/// committee, signer bitmap, VRF binding, or fee sum produce different
/// fingerprints. These trigger the contradictory-metadata fatal.
///
/// `canonical_vrf_proof_hash` is the executor-derived
/// `keccak256(VrfProof::encode())` from the verified certificate. The
/// caller obtains it from `outbe_consensus::proof::VerifiedProof::vrf_proof_hash`
/// (already validated by `verify_v2_proof` before this function runs).
///
/// V3 fingerprint encoding:
///
/// ```text
/// keccak256(
///     "OUTBE_METADATA_FINGERPRINT_V3"
///     || finalized_block_hash                    (32 bytes)
///     || finalized_block_number_be8              (8  bytes)
///     || finalized_epoch_be8                     (8  bytes)
///     || finalized_view_be8                      (8  bytes)
///     || u64_be(committee.len()) || addresses_concat   // ordered committee
///     || canonical_signer_set_hash(signer_bitmap)      (32 bytes)
///     || committee_set_hash                            (32 bytes)
///     || vrf_material_version_be8                      (8  bytes)
///     || vrf_group_public_key_hash                     (32 bytes)
///     || canonical_vrf_proof_hash                      (32 bytes)
///     || proof_kind_tag                                (1  byte)
///     || u64_be(missed_proposers.len()) || addresses_concat  // empty under V2
///     || validator_fee_sum_be32                        (32 bytes)
/// )
/// ```
///
/// The `v2_rewards_fingerprint_changes_on_*` tests assert every bound
/// field independently.
pub fn check_and_record_metadata_fingerprint(
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
    validator_fee_sum: U256,
    canonical_vrf_proof_hash: B256,
) -> Result<MetadataFingerprintOutcome> {
    let fb_hash = metadata.finalized_block_hash;
    let fp = compute_metadata_fingerprint(metadata, validator_fee_sum, canonical_vrf_proof_hash);

    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let prev = rewards.metadata_fingerprint_for_block.read(&fb_hash)?;

    if prev == B256::ZERO {
        // First time seeing this fb_hash - persist and proceed.
        rewards.metadata_fingerprint_for_block.write(&fb_hash, fp)?;
        return Ok(MetadataFingerprintOutcome::Fresh);
    }
    if prev == fp {
        return Ok(MetadataFingerprintOutcome::IdenticalReplay);
    }
    // Same fb_hash, different fingerprint: contradictory metadata.
    // The error is `Revert`. CertifiedParentAccounting rejects the block
    // because that phase fails the block on revert.
    Err(RewardsError::ContradictoryConsensusMetadata {
        fb_hash,
        stored: prev,
        new: fp,
    }
    .into())
}

/// compute the V3 fingerprint. See
/// [`check_and_record_metadata_fingerprint`] for the canonical byte
/// layout. Public for unit-test access from `tests/v2_fingerprint.rs`.
///
/// This fingerprint detects economic replay. It does not authenticate certificates.
/// V3 does not hash `parent_view`.
///
/// Consensus checks `{epoch, view, parent_view, fb_hash}` against the signed proposal
/// before it processes certified parent accounting (CPA) metadata.
/// EVM `LateFinalizeCredits` checks the stored binding, including `parent_view`,
/// when it authenticates a late credit.
pub fn compute_metadata_fingerprint(
    metadata: &CertifiedParentAccountingMetadata,
    validator_fee_sum: U256,
    canonical_vrf_proof_hash: B256,
) -> B256 {
    let mut buf: Vec<u8> = Vec::with_capacity(
        FINGERPRINT_DOMAIN.len()
            + 32 // finalized_block_hash
            + 8  // finalized_block_number
            + 8  // finalized_epoch
            + 8  // finalized_view
            + 8 + metadata.ordered_committee.len() * 20
            + 32 // canonical_signer_set_hash
            + 32 // committee_set_hash
            + 8  // vrf_material_version
            + 32 // vrf_group_public_key_hash
            + 32 // canonical_vrf_proof_hash
            + 1  // proof_kind_tag
            + 8 + metadata.missed_proposers.len() * 20
            + 32, // validator_fee_sum
    );
    buf.extend_from_slice(FINGERPRINT_DOMAIN);
    buf.extend_from_slice(metadata.finalized_block_hash.as_slice());
    buf.extend_from_slice(&metadata.finalized_block_number.to_be_bytes());
    buf.extend_from_slice(&metadata.finalized_epoch.to_be_bytes());
    buf.extend_from_slice(&metadata.finalized_view.to_be_bytes());
    write_addr_list(&mut buf, &metadata.ordered_committee);
    buf.extend_from_slice(canonical_signer_set_hash(&metadata.signer_bitmap).as_slice());
    buf.extend_from_slice(metadata.committee_set_hash.as_slice());
    buf.extend_from_slice(&metadata.vrf_material_version.to_be_bytes());
    buf.extend_from_slice(metadata.vrf_group_public_key_hash.as_slice());
    buf.extend_from_slice(canonical_vrf_proof_hash.as_slice());
    buf.push(metadata.proof_kind.tag());
    // `missed_proposers` is always empty under
    // V2 per `verify_v2_proof`. This code still keeps the length-prefixed
    // encoding, so the helper remains injective if a future
    // hard fork relaxes the V2 emptiness rule.
    buf.extend_from_slice(&(metadata.missed_proposers.len() as u64).to_be_bytes());
    for ev in &metadata.missed_proposers {
        buf.extend_from_slice(ev.validator.as_slice());
    }
    buf.extend_from_slice(&validator_fee_sum.to_be_bytes::<32>());
    keccak256(&buf)
}

fn write_addr_list(buf: &mut Vec<u8>, list: &[Address]) {
    buf.extend_from_slice(&(list.len() as u64).to_be_bytes());
    for a in list {
        buf.extend_from_slice(a.as_slice());
    }
}

/// re-export the proof-kind enum from the wire-format crate
/// so test crates can construct synthetic metadata without depending on
/// `outbe-primitives` directly. Tests use this to assert that swapping
/// `Finalization <-> CertifiedNotarization` changes the fingerprint.
pub use outbe_primitives::consensus_metadata::ParentParticipationProof as ProofKind;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{block_ctx, with_block, GENESIS_TS as GENESIS_TS_2024_01_01};

    #[test]
    fn ensure_genesis_anchor_initializes_on_first_call_and_is_idempotent() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx| {
            let day = ensure_genesis_anchor(&ctx).unwrap();
            assert_eq!(day, 20240101);
            let day_again = ensure_genesis_anchor(&ctx).unwrap();
            assert_eq!(day_again, 20240101);
        });
    }

    #[test]
    fn ensure_genesis_anchor_does_not_advance_after_lock() {
        // Lock anchor at block 0.
        with_block(0, GENESIS_TS_2024_01_01, |ctx0| {
            let _ = ensure_genesis_anchor(&ctx0).unwrap();

            // Re-call with a later-block context (same storage). Anchor stays.
            let ctx_later = BlockRuntimeContext::new(
                block_ctx(
                    100,
                    GENESIS_TS_2024_01_01 + 30 * outbe_primitives::time::SECONDS_PER_DAY,
                ),
                ctx0.storage.clone(),
            );
            let day = ensure_genesis_anchor(&ctx_later).unwrap();
            assert_eq!(day, 20240101);
            let read_back = genesis_utc_day(&ctx_later).unwrap();
            assert_eq!(read_back, 20240101);
        });
    }

    #[test]
    fn genesis_utc_day_uninitialized_is_fatal() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx| {
            let uninit_ctx = BlockRuntimeContext::new(
                outbe_primitives::block::BlockContext::new(
                    0,
                    GENESIS_TS_2024_01_01,
                    1,
                    Address::ZERO,
                    Vec::new(),
                )
                .with_genesis_timestamp(0),
                ctx.storage,
            );
            let err = genesis_utc_day(&uninit_ctx).unwrap_err();
            assert!(format!("{err}").contains("not initialized"));
        });
    }

    #[test]
    fn day_number_since_genesis_walks_forward() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx| {
            let _ = ensure_genesis_anchor(&ctx).unwrap();

            assert_eq!(day_number_since_genesis(&ctx, 20240101).unwrap(), 0);
            assert_eq!(day_number_since_genesis(&ctx, 20240131).unwrap(), 30);
            assert_eq!(day_number_since_genesis(&ctx, 20250101).unwrap(), 366); // leap
        });
    }

    #[test]
    fn day_number_since_genesis_pre_genesis_is_fatal() {
        with_block(0, GENESIS_TS_2024_01_01, |ctx| {
            let _ = ensure_genesis_anchor(&ctx).unwrap();

            let err = day_number_since_genesis(&ctx, 20231231).unwrap_err();
            assert!(format!("{err}").contains("predates genesis"));
        });
    }

    // The legacy `settle_eligible_days` / `settle_day` helpers and their
    // tests were dropped (Phase 6). Daily emission
    // orchestration lives in `outbe_cycle::handler::run_emission_limit_daily`.
    // The Cycle crate tests and the public api tests in `crate::api::tests`
    // now cover the contract.
}
