//! Per-block fee escrow + participation accumulation hook.
//!
//! The begin-zone CertifiedParentAccounting phase calls this hook.
//! The call follows the fingerprint check and `record_finalized_participation`.
//! The hook does the idempotent per-finalized-block work:
//!
//! 1. On the first finalized day, write `last_settled_utc_day` once.
//!    The hook does not advance that slot again.
//! 2. Add `validator_fee_sum` into `daily_fee_sum_raw`, guarded by
//!    `block_metadata_counted[fb_hash]`. This hook does not write
//!    `daily_fee_dust`.
//! 3. Per-block fee ESCROW + participation count, guarded by `fb_hash` /
//!    `(fb_hash, voter)` composite keys. The hook does NOT pay fees eagerly.
//!    `late_settlement::escrow_block_fee` escrows the `validator_fee_sum` of the
//!    block (`pending_fees[fb_hash]`, base 2f+1 seeded at `k=0`). The fee settles
//!    at `N+K` over the inclusion-window voter set. The daily emission top-up
//!    lands later at the day-boundary settle (step 11).
//! 4. Advance `max_observed_finalized_day` (monotonic).
//!

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    block::BlockRuntimeContext,
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::{PrecompileError, Result},
    storage::finalized_guard_ring::{FinalizedGuardRing, FINALIZED_GUARD_RETAIN},
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::schema::Rewards;

/// Number of recent finalized blocks whose per-`fb_hash` guard maps
/// (`block_metadata_counted`, `metadata_fingerprint_for_block`,
/// `fee_dust_counted_for_block`, `fee_settled`) stay live. The replay/settle
/// horizon is the K-block late-finalize window
/// ([`LATE_FINALIZE_WINDOW_K`](outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K) = 3).
/// Thus, a retention of the last 64 finalized blocks is generous. [`prune_block_guards`]
/// prunes older guard flags. A change to this value is a hard fork.
pub const BLOCK_GUARD_RETAIN: u64 = FINALIZED_GUARD_RETAIN;

/// Record `fb_hash` in the prune ring and clear the four per-`fb_hash` guard
/// maps of the finalized block evicted `BLOCK_GUARD_RETAIN` records ago.
///
/// Without this, `block_metadata_counted`, `metadata_fingerprint_for_block`,
/// `fee_dust_counted_for_block`, and `fee_settled` grow by one entry per
/// finalized block forever. The evicted block is `BLOCK_GUARD_RETAIN` >> K
/// blocks old, so nothing can re-count or settle it. Thus, clearing its
/// guards cannot weaken replay protection for any block still in the window.
/// Settlement frees the nested `participation_counted_for_block[fb_hash]` map
/// instead (see `late_settlement::settle_window`), where the credited voter
/// set is known.
fn prune_block_guards(rewards: &Rewards<'_>, fb_hash: B256) -> Result<()> {
    FinalizedGuardRing {
        entries: &rewards.block_guard_ring,
        cursor: &rewards.block_guard_ring_seq,
    }
    .record(
        fb_hash,
        |evicted| {
            rewards.block_metadata_counted.write(&evicted, false)?;
            rewards
                .metadata_fingerprint_for_block
                .write(&evicted, B256::ZERO)?;
            rewards.fee_dust_counted_for_block.write(&evicted, false)?;
            rewards.fee_settled.write(&evicted, false)
        },
        || PrecompileError::Revert("block_guard_ring_seq overflow".into()),
    )
}

/// Per-block fee escrow and participation/cap accumulation.
///
/// Caller must have already:
/// - run `check_and_record_metadata_fingerprint` and seen `Fresh`
///   (identical replay short-circuits upstream),
/// - called `record_finalized_participation`,
/// - resolved the finalized parent's `validator_fee_sum` and timestamp.
///
/// `voters` is the list of validator addresses whose `signer_bitmap`
/// bit was set. The slashing wrappers handle the absent set separately.
///
/// `validator_fee_sum` comes from
/// `finalized.summary.validator_fee_sum`. It is the raw fees
/// escrowed on `REWARDS_ADDRESS` for the finalized parent block.
///
/// `finalized_block_timestamp` is the timestamp of the finalized parent
/// block. The hook uses it to compute the UTC day key (`fb_day`).
pub fn on_finalized_metadata(
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
    validator_fee_sum: U256,
    finalized_block_timestamp: u64,
    voters: &[Address],
) -> Result<()> {
    let fb_hash = metadata.finalized_block_hash;
    let fb_day = timestamp_to_date_key(finalized_block_timestamp);

    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();

    // First sight of this finalized block? `block_metadata_counted` is the
    // durable per-`fb_hash` first-seen signal. Thus, the prune ring advances exactly
    // once per finalized block, even if the hook runs again for the same hash.
    let first_seen = !rewards.block_metadata_counted.read(&fb_hash)?;

    // 1. Lazy init of `last_settled_utc_day` on first finalized day observed.
    if rewards.last_settled_utc_day.read()? == 0 {
        rewards
            .last_settled_utc_day
            .write(previous_date_key(fb_day))?;
    }

    // The per-day raw fee accumulation still feeds the daily-emission cap.
    // But the hook now ESCROWS the fees per finalized block (it does not pay them
    // eagerly), and they settle at N+K. Idempotent via `block_metadata_counted`.
    if first_seen {
        let closes_at = metadata
            .finalized_block_number
            .checked_add(outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K)
            .ok_or_else(|| PrecompileError::Fatal("reward window height overflow".into()))?;
        rewards.pending_reward_day.write(&fb_hash, fb_day)?;
        let previous_close = rewards.daily_last_window_close.read(&fb_day)?;
        rewards
            .daily_last_window_close
            .write(&fb_day, previous_close.max(closes_at))?;
        let prev_raw = rewards.daily_fee_sum_raw.read(&fb_day)?;
        let next_raw = prev_raw
            .checked_add(validator_fee_sum)
            .ok_or_else(|| PrecompileError::Revert("daily_fee_sum_raw overflow".into()))?;
        rewards.daily_fee_sum_raw.write(&fb_day, next_raw)?;
        rewards.block_metadata_counted.write(&fb_hash, true)?;
    }

    // Per-block fee escrow (replaces the former eager per-voter
    // `transfer_balance`). Record `pending_fees[fb_hash]` and seed the base 2f+1
    // (the eager finalize signers) at inclusion distance k=0. At N+K, the
    // `LateFinalizeCredits` begin-zone phase settles the fee over the full credited
    // voter set (decay-weighted, fixed denominator). The residue burns.
    // Committee size = ordered_committee length, bounded by MAX_VALIDATORS (256).
    // Thus, it always fits u32 (the clamp is a defensive no-panic guard, never hit).
    let committee_size = u32::try_from(metadata.ordered_committee.len()).unwrap_or(u32::MAX);
    let binding = crate::late_settlement::FinalizedBlockBinding {
        number: metadata.finalized_block_number,
        hash: fb_hash,
        committee_size,
        epoch: metadata.finalized_epoch,
        view: metadata.finalized_view,
        parent_view: metadata.parent_view,
        committee_set_hash: metadata.committee_set_hash,
    };
    crate::late_settlement::escrow_block_fee(ctx, &binding, validator_fee_sum, voters)?;

    // Base and authenticated late voters share one per-block participation guard.
    for voter in voters {
        record_reward_participation(ctx, fb_hash, fb_day, *voter)?;
    }

    // 4. Advance max observed finalized day (monotonic).
    let prev_max = rewards.max_observed_finalized_day.read()?;
    if fb_day > prev_max {
        rewards.max_observed_finalized_day.write(fb_day)?;
    }

    // Bound finalized-block replay guards, well beyond the late-credit window.
    if first_seen {
        prune_block_guards(&rewards, fb_hash)?;
    }
    Ok(())
}

/// Count one verified participant, independent of whether it arrived in the
/// base certificate or in the canonical late-credit window. The reward belongs
/// to the finalized block's day, never to the credit's inclusion day.
pub(crate) fn record_reward_participation(
    ctx: &BlockRuntimeContext,
    fb_hash: B256,
    fb_day: u32,
    voter: Address,
) -> Result<()> {
    let rewards = ctx.storage.contract::<Rewards>();
    let participation_guard = rewards.participation_counted_for_block.get_nested(&fb_hash);
    let day_participation = rewards.daily_participation.get_nested(&fb_day);
    let day_voter_at = rewards.daily_voter_at.get_nested(&fb_day);

    if !participation_guard.read(&voter)? {
        if rewards.daily_topup_prepared.read(&fb_day)? {
            return Err(PrecompileError::Fatal(
                "reward participation arrived after GEM batch preparation".into(),
            ));
        }
        let prev_count = day_participation.read(&voter)?;
        if prev_count == 0 {
            // First time we see this voter for this day -> append to
            // the deterministic ordered voter list.
            let idx = rewards.daily_voter_count.read(&fb_day)?;
            day_voter_at.write(&idx, voter)?;
            let next_idx = idx
                .checked_add(1)
                .ok_or_else(|| PrecompileError::Revert("daily_voter_count overflow".into()))?;
            rewards.daily_voter_count.write(&fb_day, next_idx)?;
        }
        let next_count = prev_count
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Revert("daily_participation overflow".into()))?;
        day_participation.write(&voter, next_count)?;

        let prev_total = rewards.daily_total_participation.read(&fb_day)?;
        let next_total = prev_total
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Revert("daily_total_participation overflow".into()))?;
        rewards
            .daily_total_participation
            .write(&fb_day, next_total)?;

        participation_guard.write(&voter, true)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        block_ctx, bootstrap_genesis, fund_rewards, meta_with_hash, record_genesis_day_parent,
        with_block, with_funded_genesis_block, with_genesis_block, CHAIN_ID, FB_HASH_A, FB_HASH_B,
        GENESIS_TS, SECONDS_PER_DAY, VAL_X, VAL_Y, VAL_Z,
    };
    use alloy_primitives::B256;
    use outbe_primitives::addresses::REWARDS_ADDRESS;
    use outbe_primitives::storage::finalized_guard_ring::{test_ring_hash, RingPosition};
    use outbe_primitives::storage::hashmap::{HashMapStorageProvider, MutationPrefixViews};
    use outbe_primitives::storage::StorageHandle;

    /// Records genesis-day parent `fb_hash` at height `fb_number` with a fee
    /// of 100, voted by `VAL_X` and then `VAL_Y`.
    fn record_parent_xy(ctx: &BlockRuntimeContext, fb_hash: B256, fb_number: u64) {
        record_genesis_day_parent(
            ctx,
            &meta_with_hash(fb_hash, fb_number),
            100,
            &[VAL_X, VAL_Y],
        );
    }

    #[test]
    fn late_vote_across_midnight_counts_once_for_the_original_reward_day() {
        with_block(11, GENESIS_TS + SECONDS_PER_DAY, |ctx| {
            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_A, 10),
                U256::ZERO,
                GENESIS_TS + SECONDS_PER_DAY - 1,
                &[VAL_X],
            )
            .unwrap();
            crate::late_settlement::record_late_credit(&ctx, FB_HASH_A, VAL_Y, 1).unwrap();
            crate::late_settlement::record_late_credit(&ctx, FB_HASH_A, VAL_Y, 2).unwrap();
            crate::late_settlement::record_late_credit(&ctx, FB_HASH_A, VAL_X, 1).unwrap();
            assert_eq!(
                crate::api::read_voters_for_day(&ctx, 20240101).unwrap(),
                vec![(VAL_X, 1), (VAL_Y, 1)]
            );
            assert!(crate::api::read_voters_for_day(&ctx, 20240102)
                .unwrap()
                .is_empty());
        });
    }

    #[test]
    fn escrows_block_fees_and_seeds_base_voters_at_k0() {
        with_genesis_block(|ctx| {
            let fees = U256::from(101u64);
            fund_rewards(&ctx, fees);

            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_A, 1),
                fees,
                GENESIS_TS,
                &[VAL_X, VAL_Y],
            )
            .unwrap();

            // Fees are ESCROWED, not paid eagerly. Voter balances stay
            // zero and the full fee remains on REWARDS until settle at N+K.
            assert_eq!(ctx.storage.balance(VAL_X).unwrap(), U256::ZERO);
            assert_eq!(ctx.storage.balance(VAL_Y).unwrap(), U256::ZERO);
            assert_eq!(ctx.storage.balance(REWARDS_ADDRESS).unwrap(), fees);

            let rewards = ctx.storage.contract::<Rewards>();
            assert_eq!(rewards.pending_fees.read(&FB_HASH_A).unwrap(), fees);
            assert!(!rewards.fee_settled.read(&FB_HASH_A).unwrap());
            // Base 2f+1 seeded at k=0 (stored k+1 == 1).
            let kmap = rewards.late_voter_k_plus1.get_nested(&FB_HASH_A);
            assert_eq!(kmap.read(&VAL_X).unwrap(), 1);
            assert_eq!(kmap.read(&VAL_Y).unwrap(), 1);
            assert_eq!(rewards.late_voter_count.read(&FB_HASH_A).unwrap(), 2);
            // Daily raw fee accounting (emission-cap input) still accumulates.
            assert_eq!(rewards.daily_fee_sum_raw.read(&20240101).unwrap(), fees);
        });
    }

    #[test]
    fn records_per_voter_participation_and_voter_list() {
        with_funded_genesis_block(100, |ctx| {
            record_parent_xy(&ctx, FB_HASH_A, 1);

            let rewards = ctx.storage.contract::<Rewards>();
            let day_participation = rewards.daily_participation.get_nested(&20240101);
            assert_eq!(day_participation.read(&VAL_X).unwrap(), 1);
            assert_eq!(day_participation.read(&VAL_Y).unwrap(), 1);
            assert_eq!(
                rewards.daily_total_participation.read(&20240101).unwrap(),
                2
            );
            assert_eq!(rewards.daily_voter_count.read(&20240101).unwrap(), 2);

            let day_voter_at = rewards.daily_voter_at.get_nested(&20240101);
            // First-seen order: VAL_X at idx 0, VAL_Y at idx 1.
            assert_eq!(day_voter_at.read(&0u32).unwrap(), VAL_X);
            assert_eq!(day_voter_at.read(&1u32).unwrap(), VAL_Y);
        });
    }

    #[test]
    fn replay_for_same_fb_hash_is_idempotent() {
        with_funded_genesis_block(100, |ctx| {
            record_parent_xy(&ctx, FB_HASH_A, 1);

            // Replay: escrow, base-voter seeding, raw-fee and participation are
            // all idempotent.
            record_parent_xy(&ctx, FB_HASH_A, 1);

            // No eager payout. The escrow holds the full fee once.
            assert_eq!(ctx.storage.balance(VAL_X).unwrap(), U256::ZERO);
            assert_eq!(ctx.storage.balance(VAL_Y).unwrap(), U256::ZERO);

            let rewards = ctx.storage.contract::<Rewards>();
            assert_eq!(
                rewards.pending_fees.read(&FB_HASH_A).unwrap(),
                U256::from(100u64)
            );
            assert_eq!(
                rewards.late_voter_count.read(&FB_HASH_A).unwrap(),
                2,
                "replay must not duplicate base voters"
            );
            assert_eq!(
                rewards.daily_fee_sum_raw.read(&20240101).unwrap(),
                U256::from(100u64),
                "replay must not double-count raw fees"
            );
            assert_eq!(
                rewards.daily_total_participation.read(&20240101).unwrap(),
                2,
                "replay must not double-count participation"
            );
        });
    }

    #[test]
    fn distinct_fb_hashes_for_same_day_aggregate() {
        with_funded_genesis_block(200, |ctx| {
            record_parent_xy(&ctx, FB_HASH_A, 1);
            record_parent_xy(&ctx, FB_HASH_B, 2);

            let rewards = ctx.storage.contract::<Rewards>();
            // Both blocks contributed 100 -> 200 raw (emission-cap input).
            assert_eq!(
                rewards.daily_fee_sum_raw.read(&20240101).unwrap(),
                U256::from(200u64)
            );
            // Each finalized block is escrowed separately under its own fb_hash.
            assert_eq!(
                rewards.pending_fees.read(&FB_HASH_A).unwrap(),
                U256::from(100u64)
            );
            assert_eq!(
                rewards.pending_fees.read(&FB_HASH_B).unwrap(),
                U256::from(100u64)
            );
            // No eager payouts - fees stay escrowed until settle.
            assert_eq!(ctx.storage.balance(VAL_X).unwrap(), U256::ZERO);
            assert_eq!(ctx.storage.balance(VAL_Y).unwrap(), U256::ZERO);
            // Participation: 2 blocks x 2 voters = 4.
            assert_eq!(
                rewards.daily_total_participation.read(&20240101).unwrap(),
                4
            );
            assert_eq!(
                rewards
                    .daily_participation
                    .get_nested(&20240101)
                    .read(&VAL_X)
                    .unwrap(),
                2
            );
        });
    }

    #[test]
    fn first_call_initializes_last_settled_utc_day_to_previous_day() {
        with_genesis_block(|ctx| {
            let rewards = ctx.storage.contract::<Rewards>();
            assert_eq!(rewards.last_settled_utc_day.read().unwrap(), 0);

            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_A, 1),
                U256::ZERO,
                GENESIS_TS, // fb_day = 20240101
                &[VAL_X],
            )
            .unwrap();

            // Initialized to previous_date_key(20240101) = 20231231.
            assert_eq!(rewards.last_settled_utc_day.read().unwrap(), 20231231);
            assert_eq!(rewards.max_observed_finalized_day.read().unwrap(), 20240101);
        });
    }

    #[test]
    fn metadata_for_settled_day_is_not_fatal_under_sync_phase_ordering() {
        with_genesis_block(|ctx| {
            // `daily_settled` is a Cycle-owned completion marker.
            // makes finalized metadata synchronous Phase 1 input before Cycle
            // runs, so the old late-after-settle fatal guard is gone.
            ctx.storage
                .contract::<Rewards>()
                .daily_settled
                .write(&20240101, true)
                .unwrap();

            record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 0, &[VAL_X]);

            let rewards = ctx.storage.contract::<Rewards>();
            assert!(rewards.block_metadata_counted.read(&FB_HASH_A).unwrap());
            assert_eq!(
                rewards
                    .daily_participation
                    .get_nested(&20240101)
                    .read(&VAL_X)
                    .unwrap(),
                1
            );
        });
    }

    #[test]
    fn no_voters_escrows_full_fee_with_no_base_seed() {
        with_genesis_block(|ctx| {
            record_genesis_day_parent(&ctx, &meta_with_hash(FB_HASH_A, 1), 100, &[]);

            let rewards = ctx.storage.contract::<Rewards>();
            assert_eq!(
                rewards.daily_fee_sum_raw.read(&20240101).unwrap(),
                U256::from(100u64)
            );
            // Whole fee escrowed. No base voters -> the entire pool becomes
            // burnable residue at settle.
            assert_eq!(
                rewards.pending_fees.read(&FB_HASH_A).unwrap(),
                U256::from(100u64)
            );
            assert_eq!(rewards.late_voter_count.read(&FB_HASH_A).unwrap(), 0);
            assert_eq!(
                rewards.daily_total_participation.read(&20240101).unwrap(),
                0
            );
        });
    }

    #[test]
    fn cross_day_block_advances_max_observed_without_settle() {
        // on_finalized_metadata does not trigger day-boundary
        // settlement. The hook only updates the per-day fee
        // accumulators, per-voter participation, and the monotonic
        // `max_observed_finalized_day` watermark. The new Cycle orchestrator owns
        // the day-boundary settle. The settle fires via
        // `crate::api::prepare_daily_validator_gem_batch`, not from this hook.
        with_funded_genesis_block(200, |ctx| {
            // Block from day D=20240101.
            record_parent_xy(&ctx, FB_HASH_A, 1);
            // Block from day D+2=20240103.
            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_B, 2),
                U256::from(100u64),
                GENESIS_TS + 2 * SECONDS_PER_DAY,
                &[VAL_X, VAL_Y],
            )
            .unwrap();

            let rewards = ctx.storage.contract::<Rewards>();
            // max_observed_finalized_day still advances monotonically:
            // it is the watermark consumed by the orchestrator.
            assert_eq!(rewards.max_observed_finalized_day.read().unwrap(), 20240103);
            // No settle: every day's marker stays false until the
            // orchestrator runs.
            assert!(!rewards.daily_settled.read(&20240101).unwrap());
            assert!(!rewards.daily_settled.read(&20240102).unwrap());
            assert!(!rewards.daily_settled.read(&20240103).unwrap());
            // The hook lazy-initializes `last_settled_utc_day` on the very
            // first observed finalized day to `previous_date_key(fb_day)`.
            // The hook no longer advances it.
            assert_eq!(rewards.last_settled_utc_day.read().unwrap(), 20231231);
        });
    }

    // -- Step 23: idempotency property test -----------------------------
    //
    // Replay-safety contract:
    // 1. Apply a canonical sequence of finalized metadata events.
    // 2. Re-apply any subset of those events any number of times.
    // 3. The resulting state must be byte-equal to the canonical-only baseline.
    // This is the structural justification for the removal of the
    // `<= applied_number` watermark in step 12.
    //
    // The test only exercises *duplicate* replays of already-applied
    // events. The test intentionally does NOT cover re-ordering of distinct
    // events. `daily_voter_at[day][i]` records first-seen order, so a different
    // order produces a different (still valid) storage layout.

    use proptest::prelude::*;

    /// Snapshot of every Rewards slot the hook may touch, plus voter and
    /// REWARDS balances. Equality on this struct is the byte-equality
    /// contract the watermark-removal proof relies on.
    #[derive(Debug, PartialEq, Eq)]
    struct ReplaySnapshot {
        last_settled_utc_day: u32,
        max_observed_finalized_day: u32,
        rewards_balance: U256,
        // Per-day aggregates.
        daily_fee_sum_raw: std::collections::BTreeMap<u32, U256>,
        daily_fees_paid: std::collections::BTreeMap<u32, U256>,
        daily_fee_dust: std::collections::BTreeMap<u32, U256>,
        daily_total_participation: std::collections::BTreeMap<u32, u64>,
        daily_voter_count: std::collections::BTreeMap<u32, u32>,
        daily_voter_at: std::collections::BTreeMap<(u32, u32), Address>,
        daily_participation_per_voter: std::collections::BTreeMap<(u32, Address), u64>,
        // Per-fb_hash guards.
        block_metadata_counted: std::collections::BTreeMap<B256, bool>,
        // Per-voter balances.
        voter_balances: std::collections::BTreeMap<Address, U256>,
    }

    fn snapshot(
        ctx: &BlockRuntimeContext,
        days: &[u32],
        voters: &[Address],
        fb_hashes: &[B256],
    ) -> ReplaySnapshot {
        let rewards = ctx.storage.contract::<Rewards>();
        let mut daily_fee_sum_raw = std::collections::BTreeMap::new();
        let mut daily_fees_paid = std::collections::BTreeMap::new();
        let mut daily_fee_dust = std::collections::BTreeMap::new();
        let mut daily_total_participation = std::collections::BTreeMap::new();
        let mut daily_voter_count = std::collections::BTreeMap::new();
        let mut daily_voter_at = std::collections::BTreeMap::new();
        let mut daily_participation_per_voter = std::collections::BTreeMap::new();
        for &d in days {
            daily_fee_sum_raw.insert(d, rewards.daily_fee_sum_raw.read(&d).unwrap());
            daily_fees_paid.insert(d, rewards.daily_fees_paid.read(&d).unwrap());
            daily_fee_dust.insert(d, rewards.daily_fee_dust.read(&d).unwrap());
            daily_total_participation
                .insert(d, rewards.daily_total_participation.read(&d).unwrap());
            let count = rewards.daily_voter_count.read(&d).unwrap();
            daily_voter_count.insert(d, count);
            let voter_at = rewards.daily_voter_at.get_nested(&d);
            for i in 0..count {
                daily_voter_at.insert((d, i), voter_at.read(&i).unwrap());
            }
            let participation = rewards.daily_participation.get_nested(&d);
            for &v in voters {
                daily_participation_per_voter.insert((d, v), participation.read(&v).unwrap());
            }
        }
        let mut block_metadata_counted = std::collections::BTreeMap::new();
        for &h in fb_hashes {
            block_metadata_counted.insert(h, rewards.block_metadata_counted.read(&h).unwrap());
        }
        let mut voter_balances = std::collections::BTreeMap::new();
        for &v in voters {
            voter_balances.insert(v, ctx.storage.balance(v).unwrap());
        }
        ReplaySnapshot {
            last_settled_utc_day: rewards.last_settled_utc_day.read().unwrap(),
            max_observed_finalized_day: rewards.max_observed_finalized_day.read().unwrap(),
            rewards_balance: ctx.storage.balance(REWARDS_ADDRESS).unwrap(),
            daily_fee_sum_raw,
            daily_fees_paid,
            daily_fee_dust,
            daily_total_participation,
            daily_voter_count,
            daily_voter_at,
            daily_participation_per_voter,
            block_metadata_counted,
            voter_balances,
        }
    }

    /// One canonical event in a replay scenario.
    #[derive(Debug, Clone)]
    struct Event {
        fb_hash: B256,
        fb_number: u64,
        fb_timestamp: u64,
        fees: U256,
        voter_mask: [bool; 3], // which of (VAL_X, VAL_Y, VAL_Z) signed
    }

    fn voters_for(mask: [bool; 3]) -> Vec<Address> {
        let pool = [VAL_X, VAL_Y, VAL_Z];
        pool.iter()
            .zip(mask.iter())
            .filter_map(|(v, &m)| if m { Some(*v) } else { None })
            .collect()
    }

    fn fb_hash_for(idx: u8) -> B256 {
        let mut bytes = [0u8; 32];
        bytes[31] = idx + 1;
        B256::from(bytes)
    }

    /// Apply a canonical event sequence to a fresh storage and return the
    /// snapshot. Replay schedule = list of indices into `events`. The function
    /// re-applies each listed event (in order, as duplicates) immediately after
    /// the canonical event at the same index. The schedule is empty for
    /// the baseline run.
    fn run_scenario(
        events: &[Event],
        replay_after: &std::collections::BTreeMap<usize, u32>,
    ) -> ReplaySnapshot {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        let mut snap = None;
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
            bootstrap_genesis(&ctx);
            // Pre-fund REWARDS with the canonical total fees (replays are
            // no-ops, so they cannot consume additional balance).
            let total_fees: U256 = events.iter().map(|e| e.fees).fold(U256::ZERO, |a, b| a + b);
            fund_rewards(&ctx, total_fees);

            for (i, e) in events.iter().enumerate() {
                let voters = voters_for(e.voter_mask);
                on_finalized_metadata(
                    &ctx,
                    &meta_with_hash(e.fb_hash, e.fb_number),
                    e.fees,
                    e.fb_timestamp,
                    &voters,
                )
                .unwrap();
                if let Some(&n) = replay_after.get(&i) {
                    for _ in 0..n {
                        on_finalized_metadata(
                            &ctx,
                            &meta_with_hash(e.fb_hash, e.fb_number),
                            e.fees,
                            e.fb_timestamp,
                            &voters,
                        )
                        .unwrap();
                    }
                }
            }

            let days: Vec<u32> = events
                .iter()
                .map(|e| outbe_primitives::time::timestamp_to_date_key(e.fb_timestamp))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let voters: Vec<Address> = vec![VAL_X, VAL_Y, VAL_Z];
            let fb_hashes: Vec<B256> = events.iter().map(|e| e.fb_hash).collect();
            snap = Some(snapshot(&ctx, &days, &voters, &fb_hashes));
        });
        snap.expect("snapshot must be captured inside enter()")
    }

    fn arb_event(idx: u8) -> impl Strategy<Value = Event> {
        // All 4 events share UTC day 20240101 (timestamps within
        // [GENESIS_TS, GENESIS_TS + 86_400)). This isolates the
        // replay-idempotency property from the late-after-settle guard.
        // Out-of-order arrivals across UTC days are a *separate* contract.
        // `late_metadata_after_settle_is_fatal` already covers it.
        (any::<u8>(), 0u64..86_400u64)
            .prop_filter("at least one voter", |(m, _)| m & 0b111 != 0)
            .prop_map(move |(m, offset)| Event {
                fb_hash: fb_hash_for(idx),
                fb_number: idx as u64 + 1,
                fb_timestamp: GENESIS_TS + offset,
                // Fees in [0, 1785]; fee=0 is a legal "no-fee" block.
                fees: U256::from((m as u64) * 7),
                voter_mask: [(m & 0b001) != 0, (m & 0b010) != 0, (m & 0b100) != 0],
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            ..ProptestConfig::default()
        })]

        /// Canonical sequence of 4 events, with arbitrary duplicate-replay
        /// counts (0..=3) inserted after each. The replay-augmented run
        /// must produce a state byte-equal to the canonical-only baseline.
        #[test]
        fn replay_idempotency_property(
            ev0 in arb_event(0),
            ev1 in arb_event(1),
            ev2 in arb_event(2),
            ev3 in arb_event(3),
            replays in proptest::collection::vec(0u32..=3, 4),
        ) {
            let events = vec![ev0, ev1, ev2, ev3];
            let mut schedule = std::collections::BTreeMap::new();
            for (i, &n) in replays.iter().enumerate() {
                if n > 0 {
                    schedule.insert(i, n);
                }
            }

            let baseline = run_scenario(&events, &std::collections::BTreeMap::new());
            let with_replays = run_scenario(&events, &schedule);

            prop_assert_eq!(
                baseline,
                with_replays,
                "replay-augmented state must equal canonical-only state"
            );
        }
    }

    /// The fingerprint that the seeded guards of a block carry.
    const SEEDED_FINGERPRINT: B256 = B256::repeat_byte(0xFE);

    /// The four per-block guards of one block and the ring position.
    #[derive(Debug, PartialEq)]
    struct BlockRingView {
        metadata_counted: bool,
        fingerprint: B256,
        dust_counted: bool,
        settled: bool,
        ring: RingPosition,
    }

    /// The guards of a block in `guards_live` state, with `entry` and `seq`.
    fn block_ring_view(guards_live: [bool; 4], entry: B256, seq: u64) -> BlockRingView {
        BlockRingView {
            metadata_counted: guards_live[0],
            fingerprint: if guards_live[1] {
                SEEDED_FINGERPRINT
            } else {
                B256::ZERO
            },
            dust_counted: guards_live[2],
            settled: guards_live[3],
            ring: RingPosition { entry, seq },
        }
    }

    /// Storage after seeding the ring cursor `seq`, the entry at
    /// `seq % RETAIN` and the four guards of every block in `guarded`.
    fn seeded_block_ring(seq: u64, entry: B256, guarded: &[B256]) -> HashMapStorageProvider {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.enter(|storage| {
            let rewards = Rewards::new(storage);
            block_guard_ring(&rewards)
                .seed_for_test(seq, entry)
                .unwrap();
            for hash in guarded {
                rewards.block_metadata_counted.write(hash, true).unwrap();
                rewards
                    .metadata_fingerprint_for_block
                    .write(hash, SEEDED_FINGERPRINT)
                    .unwrap();
                rewards
                    .fee_dust_counted_for_block
                    .write(hash, true)
                    .unwrap();
                rewards.fee_settled.write(hash, true).unwrap();
            }
        });
        provider
    }

    fn read_block_ring(
        provider: &mut HashMapStorageProvider,
        guarded: B256,
        idx: u64,
    ) -> BlockRingView {
        provider.enter(|storage| {
            let rewards = Rewards::new(storage);
            BlockRingView {
                metadata_counted: rewards.block_metadata_counted.read(&guarded).unwrap(),
                fingerprint: rewards
                    .metadata_fingerprint_for_block
                    .read(&guarded)
                    .unwrap(),
                dust_counted: rewards.fee_dust_counted_for_block.read(&guarded).unwrap(),
                settled: rewards.fee_settled.read(&guarded).unwrap(),
                ring: block_guard_ring(&rewards).position_for_test(idx).unwrap(),
            }
        })
    }

    fn block_guard_ring<'a, 's>(rewards: &'a Rewards<'s>) -> FinalizedGuardRing<'a, 's> {
        FinalizedGuardRing {
            entries: &rewards.block_guard_ring,
            cursor: &rewards.block_guard_ring_seq,
        }
    }

    fn prune(storage: StorageHandle, fb_hash: B256) -> Result<()> {
        prune_block_guards(&Rewards::new(storage), fb_hash)
    }

    /// Characterizes the write order of one prune at a wrapped cursor: the
    /// four guards of the evicted block in schema order, then the ring entry,
    /// then the cursor. A failure before write `n` leaves exactly the first
    /// `n` writes applied.
    #[test]
    fn block_guard_ring_write_order_at_a_wrapped_cursor() {
        let evicted = test_ring_hash(1);
        let fb_hash = test_ring_hash(2);
        let seq = BLOCK_GUARD_RETAIN + 5;
        let views = HashMapStorageProvider::mutation_prefix_views(
            || seeded_block_ring(seq, evicted, &[evicted]),
            |storage| prune(storage, fb_hash),
            |provider| read_block_ring(provider, evicted, 5),
        )
        .unwrap();
        assert_eq!(
            views,
            MutationPrefixViews {
                before_mutation: vec![
                    block_ring_view([true, true, true, true], evicted, seq),
                    block_ring_view([false, true, true, true], evicted, seq),
                    block_ring_view([false, false, true, true], evicted, seq),
                    block_ring_view([false, false, false, true], evicted, seq),
                    block_ring_view([false, false, false, false], evicted, seq),
                    block_ring_view([false, false, false, false], fb_hash, seq),
                ],
                complete: block_ring_view([false, false, false, false], fb_hash, seq + 1),
                mutations: 6,
            }
        );
    }

    /// At the last cursor value the prune still evicts and writes the ring
    /// entry. Then it reverts with the original message and leaves the cursor
    /// unchanged.
    #[test]
    fn block_guard_ring_cursor_overflow_reverts_after_the_ring_write() {
        let evicted = test_ring_hash(1);
        let fb_hash = test_ring_hash(2);
        let mut provider = seeded_block_ring(u64::MAX, evicted, &[evicted]);
        let result = provider.enter(|storage| prune(storage, fb_hash));
        assert!(
            matches!(&result, Err(PrecompileError::Revert(message)) if message == "block_guard_ring_seq overflow"),
            "{result:?}"
        );
        assert_eq!(
            read_block_ring(&mut provider, evicted, u64::MAX % BLOCK_GUARD_RETAIN),
            block_ring_view([false, false, false, false], fb_hash, u64::MAX)
        );
    }
}
