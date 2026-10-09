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
    error::{PrecompileError, Result},
    time::{day_number_between, timestamp_to_date_key, TimeError},
};

use crate::schema::Rewards;

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
pub fn ensure_genesis_anchor(ctx: &BlockRuntimeContext) -> Result<u32> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let day = rewards.genesis_utc_day.read()?;
    if day != 0 {
        return Ok(day);
    }
    let init_day = timestamp_to_date_key(ctx.block.timestamp);
    rewards.genesis_utc_day.write(init_day)?;
    Ok(init_day)
}

/// Reads the locked-in genesis UTC day. Returns `Revert` if the slot is
/// uninitialized. That happens only when `ensure_genesis_anchor` has not
/// run for this chain, so the lifecycle is misconfigured.
pub fn genesis_utc_day(ctx: &BlockRuntimeContext) -> Result<u32> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let day = rewards.genesis_utc_day.read()?;
    if day == 0 {
        return Err(PrecompileError::Revert(
            "Rewards.genesis_utc_day not initialized - \
             RewardsLifecycle::begin_block did not run on block 0"
                .into(),
        ));
    }
    Ok(day)
}

/// Computes the integer day number of `utc_day` relative to the chain's
/// genesis day. Returns:
/// - `Ok(0)` for the genesis day itself.
/// - `Ok(n)` for `n` days after genesis.
/// - `Revert` for a `utc_day` strictly before genesis. A finalized block
///   that predates genesis is a protocol violation.
pub fn day_number_since_genesis(ctx: &BlockRuntimeContext, utc_day: u32) -> Result<u32> {
    let genesis = genesis_utc_day(ctx)?;
    day_number_between(genesis, utc_day).map_err(|e| match e {
        TimeError::PreGenesis {
            utc_day,
            genesis_utc_day,
        } => PrecompileError::Revert(format!(
            "finalized block predates genesis: utc_day={utc_day}, \
             genesis_utc_day={genesis_utc_day}"
        )),
        // TimeError is #[non_exhaustive]. Unknown variants return `Revert`
        // so a future variant does not pass silently.
        _ => PrecompileError::Revert(format!("time helper error: {e}")),
    })
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
    Err(PrecompileError::Revert(format!(
        "contradictory consensus metadata for fb_hash={fb_hash}: \
         stored fingerprint={prev}, new fingerprint={fp}"
    )))
}

/// compute the V3 fingerprint. See
/// [`check_and_record_metadata_fingerprint`] for the canonical byte
/// layout. Public for unit-test access from `tests/v2_fingerprint.rs`.
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

/// Validator emission percentage (kept for documentation/compat). The
/// closed-form `day_emission_limit` in
/// `outbe_emissionlimit::day_emission` is the authoritative source
/// for the validator daily reward, allocated through the Cycle handler.
pub const VALIDATOR_REWARD_PERCENT: u64 = 4;

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
                block_ctx(100, GENESIS_TS_2024_01_01 + 86_400 * 30),
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
            let err = genesis_utc_day(&ctx).unwrap_err();
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
