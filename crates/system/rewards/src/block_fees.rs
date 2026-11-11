//! per-block fee escrow + inclusion-window settlement (Phase 6).
//!
//! The fees of block `N` are **escrowed** (recorded, not paid eagerly). When the
//! `K`-block inclusion window closes at `N+K`, they are split across the full credited
//! voter set with a **decay-weighted, fixed-denominator** payout:
//!
//! ```text
//! payout_i = pending_fees[N] * w(k_i) / D       D = committee_size * w_max
//! ```
//! Payouts and escrow are native COEN minor units (wei; 10^18 per COEN).
//! One six-decimal protocol minor equals 10^12 native minors; weights and D are unitless.
//!
//! `D` is constant per block (independent of who voted), so excluding a peer
//! enriches nobody. Thus there is no censorship incentive. Settlement never pays out
//! the residue (`pending - sum payout` = absentees + decay gap + division remainders):
//! `sum payout + residue == pending`.
//!
//! These are deterministic storage functions over an explicit
//! [`BlockRuntimeContext`]. The executor wires them into the begin-zone CPA phase
//! (escrow) and the `LateFinalizeCredits` phase (record + settle). At settle, the
//! shared Promis Limit helper burns the entire native residue from `REWARDS_ADDRESS`.
//! Only fractional protocol credit remains in the shared accumulator, never native
//! funds on Rewards. `transfer_block_fees` then frees per-window state; the
//! `fees_transferred` tombstone stays until guard-ring eviction.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{addresses::REWARDS_ADDRESS, block::BlockRuntimeContext, error::Result};

use crate::constants::{decay_weight, fixed_denominator};
use crate::errors::RewardsError;
use crate::precompile::IRewards;
use crate::schema::Rewards;

/// Record one credited voter for `fb_hash` at inclusion distance `k`. Keep the
/// **smallest** `k` ever seen:
/// - The first credit appends the voter to the enumerable voter list.
/// - A later, larger `k` is a no-op.
/// - A later, smaller `k` improves the stored distance without re-appending.
pub fn record_late_vote(
    ctx: &BlockRuntimeContext,
    fb_hash: B256,
    voter: Address,
    k: u8,
) -> Result<()> {
    let rewards = ctx.storage.contract::<Rewards<'_>>();
    if rewards.fees_transferred.read(&fb_hash)? {
        // Window already closed. Nothing more can be credited.
        return Ok(());
    }
    if k > outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K as u8 {
        return Err(RewardsError::CreditOutsideInclusionWindow.into());
    }
    if k > 0 {
        let day = rewards.pending_reward_day.read(&fb_hash)?;
        if day == 0 {
            return Err(RewardsError::LateCreditMissingCanonicalUtcDay.into());
        }
        crate::finalized_metadata_hook::record_reward_participation(ctx, fb_hash, day, voter)?;
    }
    let kmap = rewards.late_voter_k_plus1.get_nested(&fb_hash);
    let stored = kmap.read(&voter)?; // 0 = absent, else k+1
    if stored == 0 {
        let idx = rewards.late_voter_count.read(&fb_hash)?;
        rewards
            .late_voter_at
            .get_nested(&fb_hash)
            .write(&idx, voter)?;
        let next = idx
            .checked_add(1)
            .ok_or(RewardsError::LateVoterCountOverflow)?;
        rewards.late_voter_count.write(&fb_hash, next)?;
        kmap.write(&voter, k + 1)?;
    } else {
        let current_k = stored - 1;
        if k < current_k {
            kmap.write(&voter, k + 1)?;
        }
    }
    Ok(())
}

/// Canonical binding of one finalized block whose fees wait for settlement.
///
/// The Late phase authenticates each credit against this binding. It rejects any
/// credit whose aggregate covers a non-canonical view of the same `hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizedBlockBinding {
    /// Finalized block number `N`.
    pub number: u64,
    /// Finalized block hash. It keys the escrow and the credited voter set.
    pub hash: B256,
    /// Number of committee members. It sets the fixed payout denominator.
    pub committee_size: u32,
    pub epoch: u64,
    pub view: u64,
    pub parent_view: u64,
    pub committee_set_hash: B256,
}

/// Escrow block `N`'s fees (key `block.hash`) and seed the base 2f+1 CPA signers at
/// `k = 0`. Thus a later re-inclusion of a base voter at `k >= 1` cannot worsen its
/// distance. Idempotent: re-escrowing the same block is a no-op once settled.
/// Re-seeding a base voter is a no-op (smallest-k rule).
/// `fee_sum` is native wei. The caller must checkpoint state and logs for rollback.
pub fn escrow_block_fee(
    ctx: &BlockRuntimeContext,
    block: &FinalizedBlockBinding,
    fee_sum: U256,
    base_voters: &[Address],
) -> Result<()> {
    let mut rewards = ctx.storage.contract::<Rewards<'_>>();
    if rewards.fees_transferred.read(&block.hash)? {
        return Ok(());
    }
    // Binding presence, not a nonzero fee, distinguishes even zero-fee replays.
    let already_bound = rewards.pending_fb_hash_at.read(&block.number)? == block.hash;
    rewards.pending_fees.write(&block.hash, fee_sum)?;
    // Write the settle-trigger lookup by number, so block N+K can find block N's
    // escrow. Also write the canonical binding that the Late phase authenticates each
    // credit against: number -> {fb_hash, epoch, view, parent_view,
    // committee_set_hash}. This function pins the full signed binding. The Late phase
    // rejects any credit whose aggregate covers a non-canonical view of `fb_hash`.
    rewards
        .pending_fb_hash_at
        .write(&block.number, block.hash)?;
    rewards
        .pending_committee_size_at
        .write(&block.number, block.committee_size)?;
    rewards.pending_epoch_at.write(&block.number, block.epoch)?;
    rewards.pending_view_at.write(&block.number, block.view)?;
    rewards
        .pending_parent_view_at
        .write(&block.number, block.parent_view)?;
    rewards
        .pending_committee_set_hash_at
        .write(&block.number, block.committee_set_hash)?;
    for voter in base_voters {
        record_late_vote(ctx, block.hash, *voter, 0)?;
    }
    if !already_bound {
        rewards.emit(IRewards::BlockFeeEscrowed {
            fbHash: block.hash,
            fbNumber: block.number,
            feeSumNative: fee_sum,
        })?;
    }
    Ok(())
}

/// Window-close side effect that the `LateFinalizeCredits` begin-zone phase runs at
/// `current_block`. It settles the target whose inclusion window just closed
/// (`fb_number = current_block - K`), looked up by number. Returns a zero no-op when
/// that number has no escrow (for example block 0 or a block without recorded fees).
/// Returned `(distributed, residue)` amounts are native wei. The caller must
/// checkpoint state, balances and logs together, including the number-keyed cleanup.
pub fn seal_block_votes(
    ctx: &BlockRuntimeContext,
    current_block: u64,
    window_k: u64,
) -> Result<(U256, U256)> {
    let Some(fb_number) = current_block.checked_sub(window_k) else {
        return Ok((U256::ZERO, U256::ZERO));
    };
    if fb_number == 0 {
        // Block 0 produces no validator fees (settlement skips it).
        return Ok((U256::ZERO, U256::ZERO));
    }
    let rewards = ctx.storage.contract::<Rewards<'_>>();
    let fb_hash = rewards.pending_fb_hash_at.read(&fb_number)?;
    if fb_hash == B256::ZERO {
        return Ok((U256::ZERO, U256::ZERO));
    }
    let committee_size = u64::from(rewards.pending_committee_size_at.read(&fb_number)?);
    let result = transfer_block_fees(ctx, fb_hash, committee_size)?;

    // Free the number-keyed lookups after `transfer_block_fees` transfers the fees.
    // It frees the hash-keyed state. This code runs once: a replay reads
    // `pending_fb_hash_at[fb_number] == ZERO` above and returns early before it gets
    // here. `fees_transferred[fb_hash]` is the durable guard.
    rewards.pending_fb_hash_at.write(&fb_number, B256::ZERO)?;
    rewards.pending_committee_size_at.write(&fb_number, 0)?;
    rewards.pending_epoch_at.write(&fb_number, 0)?;
    rewards.pending_view_at.write(&fb_number, 0)?;
    rewards.pending_parent_view_at.write(&fb_number, 0)?;
    rewards
        .pending_committee_set_hash_at
        .write(&fb_number, B256::ZERO)?;
    Ok(result)
}

/// Settle the matured window for `fb_hash` exactly once:
/// 1. Pay each credited voter `pending * w(k_i) / D` from `REWARDS_ADDRESS`.
/// 2. Burn the entire native residue through the shared Promis Limit helper.
/// 3. Assert `sum payout + residue == pending`.
///
/// Returns `(distributed, residue)` in native wei. The caller must checkpoint
/// state, balances and logs together for rollback on any error.
pub fn transfer_block_fees(
    ctx: &BlockRuntimeContext,
    fb_hash: B256,
    committee_size: u64,
) -> Result<(U256, U256)> {
    let mut rewards = ctx.storage.contract::<Rewards<'_>>();
    if rewards.fees_transferred.read(&fb_hash)? {
        return Ok((U256::ZERO, U256::ZERO));
    }

    let pending = rewards.pending_fees.read(&fb_hash)?;
    let denominator = fixed_denominator(committee_size);
    if denominator.is_zero() {
        return Err(RewardsError::BlockFeeEmptyCommittee.into());
    }
    let window = MaturedWindow {
        fb_hash,
        pending,
        denominator,
        voter_count: rewards.late_voter_count.read(&fb_hash)?,
    };

    let distributed = pay_credited_voters(ctx, &mut rewards, &window)?;

    // Solvency: full attendance pays at most the pool (D = N*w_max), so
    // distributed <= pending. checked_sub guards any violation.
    let residue = pending
        .checked_sub(distributed)
        .ok_or(RewardsError::SumOfVoterPortionsExceedsBlockFees)?;
    outbe_promislimit::PromisLimitContract::new(ctx.storage.clone()).burn_to_promis_limit(
        REWARDS_ADDRESS,
        residue,
        outbe_promislimit::runtime::BurnSource::BlockFee,
    )?;

    // Parity invariant (checked once): sum payout + residue == pending.
    if distributed
        .checked_add(residue)
        .ok_or(RewardsError::BlockFeeSumOverflow)?
        != pending
    {
        return Err(RewardsError::BlockFeeInvariantMismatch.into());
    }

    rewards.fees_transferred.write(&fb_hash, true)?;
    free_window_state(&rewards, &window)?;
    rewards.emit(IRewards::BlockFeesTransferred {
        fbHash: fb_hash,
        transferredNative: distributed,
        restNative: residue,
    })?;

    Ok((distributed, residue))
}

/// Escrow state of one window at settlement.
struct MaturedWindow {
    fb_hash: B256,
    /// Escrowed fees of the window in native wei.
    pending: U256,
    /// Fixed payout denominator `D = committee_size * w_max`.
    denominator: U256,
    /// Number of credited voters, read once before the payouts.
    voter_count: u32,
}

/// Pays each credited voter of `window` its decay-weighted share of the escrow.
/// Returns the total paid amount in native wei.
fn pay_credited_voters(
    ctx: &BlockRuntimeContext,
    rewards: &mut Rewards<'_>,
    window: &MaturedWindow,
) -> Result<U256> {
    let kmap = rewards.late_voter_k_plus1.get_nested(&window.fb_hash);
    let at = rewards.late_voter_at.get_nested(&window.fb_hash);

    let mut distributed = U256::ZERO;
    for idx in 0..window.voter_count {
        let voter = at.read(&idx)?;
        let stored = kmap.read(&voter)?;
        if stored == 0 {
            continue;
        }
        let weight = decay_weight(u64::from(stored - 1));
        if weight.is_zero() {
            continue;
        }
        let payout = window
            .pending
            .checked_mul(weight)
            .ok_or(RewardsError::BlockFeePortionOverflow)?
            / window.denominator;
        if payout.is_zero() {
            continue;
        }
        ctx.storage
            .transfer_balance(REWARDS_ADDRESS, voter, payout)?;
        distributed = distributed
            .checked_add(payout)
            .ok_or(RewardsError::BlockFeeSumOverflow)?;
        rewards.emit(IRewards::VoterFeeTransferred {
            fbHash: window.fb_hash,
            voter,
            amountNative: payout,
        })?;
    }
    Ok(distributed)
}

/// Frees the per-window state of a settled window.
fn free_window_state(rewards: &Rewards<'_>, window: &MaturedWindow) -> Result<()> {
    let kmap = rewards.late_voter_k_plus1.get_nested(&window.fb_hash);
    let at = rewards.late_voter_at.get_nested(&window.fb_hash);
    // Free the per-window state after settlement. After `fees_transferred = true`,
    // `record_late_vote` short-circuits. Thus no caller writes more entries for
    // this `fb_hash`, and the data is dead. Freeing it here prevents unbounded state
    // growth. `fees_transferred` is the immediate double-transfer / re-escrow guard.
    // The ring in `record_votes_and_fees` prunes the guard itself later
    // (`BLOCK_GUARD_RETAIN` blocks after the hook first counts the block, long past
    // the K-block window). Thus the ring bounds it and prevents permanent retention.
    //
    // This code frees the nested `participation_counted_for_block[fb_hash]` map
    // too. `late_voter_at` is a superset of the participation-counted voters:
    // `escrow_block_fee` seeds every base voter passed to it into `late_voter_at`
    // at k=0. Thus clearing the guard for each credited voter clears every entry.
    // For any non-counted late voter, the clear is a harmless write of the default
    // `false`.
    let participation_guard = rewards
        .participation_counted_for_block
        .get_nested(&window.fb_hash);
    for idx in 0..window.voter_count {
        let voter = at.read(&idx)?;
        kmap.write(&voter, 0)?;
        at.write(&idx, Address::ZERO)?;
        participation_guard.write(&voter, false)?;
    }
    rewards.late_voter_count.write(&window.fb_hash, 0)?;
    rewards.pending_fees.write(&window.fb_hash, U256::ZERO)?;
    rewards.pending_reward_day.write(&window.fb_hash, 0)?;

    Ok(())
}

/// Canonical binding + full credited voter set for the window maturing at
/// `fb_number`, used by the begin-zone window-close miss/slashing pass.
///
/// `credited` is the union of base voters (k=0, seeded at escrow) and late voters
/// (k>=1) - i.e. every committee member who voted within `K`. Callers compute the
/// absentee set as `committee(fb_number) \ credited`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCloseInfo {
    pub fb_hash: B256,
    pub epoch: u64,
    pub committee_set_hash: B256,
    pub credited: Vec<Address>,
}

/// Read the [`WindowCloseInfo`] for the window that matures at `fb_number`, or
/// `None` when that number has no escrow (block 0, or a block with no fees).
///
/// Pure read over committed chain state - deterministic across proposer and
/// validator. **Must run before [`seal_block_votes`]**, which frees `late_voter_*`.
pub fn window_close_credited(
    ctx: &BlockRuntimeContext,
    fb_number: u64,
) -> Result<Option<WindowCloseInfo>> {
    let rewards = ctx.storage.contract::<Rewards<'_>>();
    let fb_hash = rewards.pending_fb_hash_at.read(&fb_number)?;
    if fb_hash == B256::ZERO {
        return Ok(None);
    }
    let epoch = rewards.pending_epoch_at.read(&fb_number)?;
    let committee_set_hash = rewards.pending_committee_set_hash_at.read(&fb_number)?;
    let count = rewards.late_voter_count.read(&fb_hash)?;
    let at = rewards.late_voter_at.get_nested(&fb_hash);
    let mut credited = Vec::new();
    for idx in 0..count {
        credited.push(at.read(&idx)?);
    }
    Ok(Some(WindowCloseInfo {
        fb_hash,
        epoch,
        committee_set_hash,
        credited,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::LATE_FINALIZE_W_MAX;
    use alloy_primitives::address;
    use alloy_sol_types::SolEvent;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, MetadosisMutationPurposeTag};
    use outbe_primitives::time::WorldwideDay;
    use outbe_primitives::units::UNITS_PER_PROTOCOL_MINOR;
    use outbe_promislimit::PromisLimitContract;

    const CHAIN_ID: u64 = 1;
    const GENESIS_HASH: B256 = B256::repeat_byte(0x11);
    const CONSENSUS_KEY: [u8; 48] = [7; 48];
    const FB: B256 = B256::repeat_byte(0xAB);
    const V0: Address = address!("0x00000000000000000000000000000000000000A0");
    const V1: Address = address!("0x00000000000000000000000000000000000000A1");
    const V2: Address = address!("0x00000000000000000000000000000000000000A2");
    const V3: Address = address!("0x00000000000000000000000000000000000000A3");

    fn seed_native_runtime(storage: &mut HashMapStorageProvider) {
        storage.enter(|handle| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(handle.clone());
            validators.set_config_max_validators(1).unwrap();
            let minimum = U256::from(1_000u64);
            validators
                .test_seed_authoritative_min_stake(minimum)
                .unwrap();
            validators
                .register_validator(Address::ZERO, V0, &CONSENSUS_KEY)
                .unwrap();
            validators
                .test_seed_authoritative_bonded(V0, minimum)
                .unwrap();
            validators.mark_pending(V0).unwrap();
            validators.confirm_validator_ready(V0).unwrap();
            validators
                .activate_validator_via_boundary_for_test(V0)
                .unwrap();
            outbe_oracle::api::register_pair(handle, outbe_oracle::api::DAY_TYPE_PAIR).unwrap();
        });
    }

    fn run(f: impl FnOnce(&BlockRuntimeContext)) -> HashMapStorageProvider {
        let mut storage = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS_HASH);
        storage.set_block_number(156);
        seed_native_runtime(&mut storage);
        // The shared-carry fixture transfers two distinct windows in this context.
        storage.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CertifiedFinality, 2);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 156,
                    timestamp: 100,
                    chain_id: CHAIN_ID,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: 100,
                    proposer: Address::ZERO,
                    validators: Vec::new(),
                },
                handle,
            );
            // Fee-only fixtures bypass CPA. Supply the canonical day that CPA binds.
            ctx.storage
                .contract::<Rewards>()
                .pending_reward_day
                .write(&FB, 19700101)
                .unwrap();
            ctx.storage
                .with_checkpoint(|| {
                    f(&ctx);
                    Ok(())
                })
                .unwrap();
        });
        storage
    }

    /// Canonical binding of block `number` for a four-member committee in epoch 0.
    fn block_at(number: u64, hash: B256) -> FinalizedBlockBinding {
        FinalizedBlockBinding {
            number,
            hash,
            committee_size: 4,
            epoch: 0,
            view: 0,
            parent_view: 0,
            committee_set_hash: B256::ZERO,
        }
    }

    fn fund(ctx: &BlockRuntimeContext, amount: U256) {
        ctx.storage
            .increase_balance(REWARDS_ADDRESS, amount)
            .unwrap();
    }

    /// Seed carry through a different real burn source, not Rewards-owned state.
    fn seed_shared_remainder(ctx: &BlockRuntimeContext, remainder: U256) {
        ctx.storage.increase_balance(V3, remainder).unwrap();
        PromisLimitContract::new(ctx.storage.clone())
            .burn_to_promis_limit(
                V3,
                remainder,
                outbe_promislimit::runtime::BurnSource::ProposalBond,
            )
            .unwrap();
    }

    /// Full k=0 attendance pays exactly the pool. REWARDS is fully drained.
    /// Parity holds with zero residue.
    #[test]
    fn full_attendance_pays_exactly_pool_no_residue() {
        let storage = run(|ctx| {
            let committee = 4u64;
            let pending = U256::from(committee) * U256::from(1_000u64); // divisible by N
            fund(ctx, pending);
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2, V3]).unwrap();
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2, V3]).unwrap();

            let (distributed, residue) = transfer_block_fees(ctx, FB, committee).unwrap();
            assert_eq!(distributed, pending);
            assert_eq!(residue, U256::ZERO);
            for v in [V0, V1, V2, V3] {
                assert_eq!(
                    ctx.storage.balance(v).unwrap(),
                    pending / U256::from(committee)
                );
            }
            assert_eq!(ctx.storage.balance(REWARDS_ADDRESS).unwrap(), U256::ZERO);
            // Neither settlement nor escrow replay emits another economic event.
            transfer_block_fees(ctx, FB, committee).unwrap();
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0]).unwrap();
        });
        let logs: Vec<_> = storage
            .get_ordered_events()
            .iter()
            .filter(|log| log.address == REWARDS_ADDRESS)
            .collect();
        assert_eq!(logs.len(), 6);
        assert!(logs.iter().all(|log| log.address == REWARDS_ADDRESS));
        assert_eq!(logs[0].data.topics().len(), 3);
        assert_eq!(
            IRewards::BlockFeeEscrowed::decode_log_data(&logs[0].data).unwrap(),
            IRewards::BlockFeeEscrowed {
                fbHash: FB,
                fbNumber: 10,
                feeSumNative: U256::from(4_000),
            }
        );
        for (log, voter) in logs[1..5].iter().zip([V0, V1, V2, V3]) {
            assert_eq!(log.data.topics().len(), 3);
            assert_eq!(
                IRewards::VoterFeeTransferred::decode_log_data(&log.data).unwrap(),
                IRewards::VoterFeeTransferred {
                    fbHash: FB,
                    voter,
                    amountNative: U256::from(1_000),
                }
            );
        }
        assert_eq!(logs[5].data.topics().len(), 2);
        assert_eq!(
            IRewards::BlockFeesTransferred::decode_log_data(&logs[5].data).unwrap(),
            IRewards::BlockFeesTransferred {
                fbHash: FB,
                transferredNative: U256::from(4_000),
                restNative: U256::ZERO,
            }
        );
    }

    #[test]
    fn zero_fee_escrow_replay_emits_once_and_missing_window_emits_nothing() {
        let storage = run(|ctx| {
            seed_shared_remainder(ctx, U256::from(7));
            seal_block_votes(ctx, 12, 3).unwrap();
            escrow_block_fee(ctx, &block_at(10, FB), U256::ZERO, &[V0]).unwrap();
            escrow_block_fee(ctx, &block_at(10, FB), U256::ZERO, &[V0]).unwrap();
            seal_block_votes(ctx, 13, 3).unwrap();
            seal_block_votes(ctx, 13, 3).unwrap();
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .rest_minor()
                    .unwrap(),
                U256::from(7)
            );
            assert_eq!(ctx.storage.balance(REWARDS_ADDRESS).unwrap(), U256::ZERO);
        });
        let logs = storage.get_events(REWARDS_ADDRESS);
        assert_eq!(logs.len(), 2);
        let escrow = IRewards::BlockFeeEscrowed::decode_log_data(&logs[0]).unwrap();
        assert_eq!(escrow.fbHash, FB);
        assert_eq!(escrow.fbNumber, 10);
        assert_eq!(escrow.feeSumNative, U256::ZERO);
        let summary = IRewards::BlockFeesTransferred::decode_log_data(&logs[1]).unwrap();
        assert_eq!(summary.transferredNative, U256::ZERO);
        assert_eq!(summary.restNative, U256::ZERO);
    }

    #[test]
    fn failed_settlement_checkpoint_restores_payment_state_and_logs() {
        let seed = || {
            run(|ctx| {
                let unit = UNITS_PER_PROTOCOL_MINOR;
                let pending = U256::from(24) * unit / U256::from(10);
                let remainder = U256::from(6) * unit / U256::from(10);
                seed_shared_remainder(ctx, remainder);
                fund(ctx, pending);
                escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2]).unwrap();
            })
        };
        let view = |storage: &mut HashMapStorageProvider| {
            let balances: Vec<_> = [REWARDS_ADDRESS, V0, V1, V2]
                .into_iter()
                .map(|address| storage.get_balance(address))
                .collect();
            let logs = storage.get_ordered_events().to_vec();
            let state = storage.enter(|handle| {
                let rewards = handle.contract::<Rewards>();
                (
                    rewards.pending_fees.read(&FB).unwrap(),
                    rewards.pending_fb_hash_at.read(&10).unwrap(),
                    rewards.pending_reward_day.read(&FB).unwrap(),
                    rewards.late_voter_count.read(&FB).unwrap(),
                    rewards.late_voter_at.get_nested(&FB).read(&0).unwrap(),
                    rewards
                        .late_voter_k_plus1
                        .get_nested(&FB)
                        .read(&V0)
                        .unwrap(),
                    PromisLimitContract::new(handle.clone())
                        .rest_minor()
                        .unwrap(),
                    rewards.fees_transferred.read(&FB).unwrap(),
                    PromisLimitContract::new(handle.clone())
                        .promis_limit()
                        .unwrap(),
                    (
                        rewards.pending_committee_size_at.read(&10).unwrap(),
                        rewards.pending_epoch_at.read(&10).unwrap(),
                        rewards.pending_view_at.read(&10).unwrap(),
                        rewards.pending_parent_view_at.read(&10).unwrap(),
                        rewards.pending_committee_set_hash_at.read(&10).unwrap(),
                        rewards
                            .participation_counted_for_block
                            .get_nested(&FB)
                            .read(&V0)
                            .unwrap(),
                        rewards.day_fee_sum_native.read(&19700101).unwrap(),
                        rewards.day_total_votes.read(&19700101).unwrap(),
                    ),
                )
            });
            (balances, logs, state)
        };
        let initial = view(&mut seed());
        // Inject before every write/event, including after payments, shared burn,
        // settled marking and cleanup, while retaining the caller's checkpoint.
        let outcomes = HashMapStorageProvider::mutation_prefix_views(
            seed,
            |handle| {
                let ctx = BlockRuntimeContext::new(
                    BlockContext {
                        block_gas_limit: 0,
                        user_gas_used: 0,
                        block_number: 156,
                        timestamp: 100,
                        chain_id: CHAIN_ID,
                        genesis_hash: alloy_primitives::B256::ZERO,
                        genesis_timestamp: 100,
                        proposer: Address::ZERO,
                        validators: Vec::new(),
                    },
                    handle,
                );
                ctx.storage
                    .with_checkpoint(|| seal_block_votes(&ctx, 13, 3))
            },
            view,
        )
        .unwrap();
        assert!(outcomes.mutations > 0);
        for failed in outcomes.before_mutation {
            assert_eq!(failed, initial);
        }
        assert_ne!(outcomes.complete, initial);
    }

    /// settling a window frees the nested
    /// `participation_counted_for_block[fb_hash]` guard for every credited voter,
    /// so it does not grow unbounded across finalized blocks.
    #[test]
    fn transfer_block_fees_clears_participation_guard() {
        run(|ctx| {
            let committee = 4u64;
            let pending = U256::from(committee) * U256::from(1_000u64);
            fund(ctx, pending);
            let rewards = ctx.storage.contract::<Rewards>();
            // Simulate the participation count `record_votes_and_fees` records.
            let guard = rewards.participation_counted_for_block.get_nested(&FB);
            for v in [V0, V1, V2, V3] {
                guard.write(&v, true).unwrap();
            }
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2, V3]).unwrap();

            transfer_block_fees(ctx, FB, committee).unwrap();

            let guard = rewards.participation_counted_for_block.get_nested(&FB);
            for v in [V0, V1, V2, V3] {
                assert!(
                    !guard.read(&v).unwrap(),
                    "participation guard for {v} must be freed at settlement"
                );
            }
        });
    }

    /// Excluding a voter does NOT raise anyone else's payout (fixed denominator).
    /// The absent share becomes burned residue.
    #[test]
    fn fixed_denominator_no_redistribution() {
        run(|ctx| {
            let committee = 4u64;
            let pending = U256::from(committee) * U256::from(1_000u64);
            fund(ctx, pending);
            // Only 3 of 4 voters credited (one excluded).
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2]).unwrap();

            let (distributed, residue) = transfer_block_fees(ctx, FB, committee).unwrap();
            let each = pending / U256::from(committee); // unchanged by exclusion
            assert_eq!(ctx.storage.balance(V0).unwrap(), each);
            assert_eq!(ctx.storage.balance(V1).unwrap(), each);
            assert_eq!(ctx.storage.balance(V2).unwrap(), each);
            assert_eq!(ctx.storage.balance(V3).unwrap(), U256::ZERO);
            assert_eq!(distributed, each * U256::from(3u64));
            assert_eq!(
                residue, each,
                "excluded voter's share is residue, not redistributed"
            );
            assert_eq!(distributed + residue, pending, "parity");
        });
    }

    #[test]
    fn residue_burns_all_native_and_carries_shared_fractional_credit() {
        run(|ctx| {
            let unit = outbe_primitives::units::UNITS_PER_PROTOCOL_MINOR;
            let pending = U256::from(4_000) * unit + U256::from(7);
            fund(ctx, pending);
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2]).unwrap();

            let (_, residue) = transfer_block_fees(ctx, FB, 4).unwrap();
            assert_eq!(residue, U256::from(1_000) * unit + U256::from(4));
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .promis_limit()
                    .unwrap(),
                U256::from(1_000)
            );
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .rest_minor()
                    .unwrap(),
                U256::from(4)
            );
            assert_eq!(ctx.storage.balance(REWARDS_ADDRESS).unwrap(), U256::ZERO);

            assert_eq!(
                transfer_block_fees(ctx, FB, 4).unwrap(),
                (U256::ZERO, U256::ZERO)
            );
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .rest_minor()
                    .unwrap(),
                U256::from(4)
            );
        });
    }

    #[test]
    fn residue_crosses_unit_boundary_with_preexisting_shared_remainder() {
        let storage = run(|ctx| {
            let unit = outbe_primitives::units::UNITS_PER_PROTOCOL_MINOR;
            let pending = U256::from(24) * unit / U256::from(10);
            seed_shared_remainder(ctx, unit / U256::from(2));
            let second = B256::repeat_byte(0xCD);
            let rewards = ctx.storage.contract::<Rewards>();
            rewards.pending_reward_day.write(&second, 19700101).unwrap();
            fund(ctx, pending * U256::from(2));
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2]).unwrap();
            escrow_block_fee(ctx, &block_at(11, second), pending, &[V0, V1, V2]).unwrap();
            let carry_over = || {
                PromisLimitContract::new(ctx.storage.clone())
                    .promis_limit()
                    .unwrap()
            };

            transfer_block_fees(ctx, FB, 4).unwrap();
            assert_eq!(carry_over(), U256::from(1));
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .rest_minor()
                    .unwrap(),
                unit / U256::from(10)
            );

            transfer_block_fees(ctx, second, 4).unwrap();
            assert_eq!(carry_over(), U256::from(1));
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .rest_minor()
                    .unwrap(),
                U256::from(7) * unit / U256::from(10)
            );
            assert_eq!(ctx.storage.balance(REWARDS_ADDRESS).unwrap(), U256::ZERO);
        });
        let summaries: Vec<_> = storage
            .get_events(REWARDS_ADDRESS)
            .iter()
            .filter_map(|log| IRewards::BlockFeesTransferred::decode_log_data(log).ok())
            .collect();
        assert_eq!(summaries.len(), 2);
        let unit = UNITS_PER_PROTOCOL_MINOR;
        let residue = U256::from(6) * unit / U256::from(10);
        assert_eq!(summaries[0].restNative, residue);
        assert_eq!(summaries[1].restNative, residue);
        for summary in summaries {
            assert_eq!(
                summary.transferredNative + summary.restNative,
                U256::from(24) * unit / U256::from(10)
            );
        }
    }

    #[test]
    fn block_fee_residue_waits_for_daily_limit_formation() {
        let mut storage = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS_HASH);
        storage.set_block_number(156);
        seed_native_runtime(&mut storage);
        let wwd = WorldwideDay::new(20_260_728);
        let timestamp = wwd.start_timestamp().unwrap();
        storage.set_timestamp(U256::from(timestamp));
        let expected_residue = U256::from(1_000);
        let unit = outbe_primitives::units::UNITS_PER_PROTOCOL_MINOR;
        storage.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CertifiedFinality);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 156,
                    timestamp,
                    chain_id: CHAIN_ID,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: timestamp.max(1),
                    proposer: Address::ZERO,
                    validators: Vec::new(),
                },
                handle,
            );
            let pending = U256::from(4_000) * unit;
            fund(&ctx, pending);
            escrow_block_fee(&ctx, &block_at(153, FB), pending, &[V0, V1, V2]).unwrap();

            let (distributed, residue) = ctx
                .storage
                .with_checkpoint(|| transfer_block_fees(&ctx, FB, 4))
                .unwrap();
            assert_eq!(distributed, U256::from(3_000) * unit);
            assert_eq!(residue, expected_residue * unit);
            let utc_day = outbe_primitives::time::timestamp_to_date_key(timestamp);
            assert!(
                outbe_emission::api::emission_day(ctx.storage.clone(), utc_day)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .promis_limit()
                    .unwrap(),
                expected_residue
            );
        });

        storage.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_gas_limit: 0,
                    user_gas_used: 0,
                    block_number: 156,
                    timestamp,
                    chain_id: CHAIN_ID,
                    genesis_hash: alloy_primitives::B256::ZERO,
                    genesis_timestamp: timestamp.max(1),
                    proposer: Address::ZERO,
                    validators: Vec::new(),
                },
                handle,
            );
            let daily_base = U256::from(10_000);
            let utc_day = outbe_primitives::time::timestamp_to_date_key(timestamp);
            outbe_emission::api::record_day_allocation(
                ctx.storage.clone(),
                utc_day,
                outbe_emission::api::DayAllocationAmounts {
                    emission_limit: daily_base,
                    metadosis_limit: daily_base,
                    validators: U256::ZERO,
                    waa: U256::ZERO,
                    sra: U256::ZERO,
                    cca: U256::ZERO,
                    promis_limit: U256::ZERO,
                },
            )
            .unwrap();

            let emission_day = outbe_emission::api::emission_day(ctx.storage.clone(), utc_day)
                .unwrap()
                .unwrap();
            assert_eq!(emission_day.emission_limit_minor, daily_base);
            assert_eq!(
                PromisLimitContract::new(ctx.storage.clone())
                    .promis_limit()
                    .unwrap(),
                expected_residue,
                "the residue stays in the accumulator for whichever day draws on it"
            );
        });
    }

    /// A voter first seen at k=K (weight 0) earns nothing. Its share burns.
    #[test]
    fn cliff_voter_at_k_equals_k_earns_zero() {
        let storage = run(|ctx| {
            let committee = 4u64;
            let pending = U256::from(committee) * U256::from(1_000u64);
            fund(ctx, pending);
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0]).unwrap(); // base at k=0
            record_late_vote(ctx, FB, V1, 3).unwrap(); // k = K = 3, weight 0

            let (distributed, residue) = transfer_block_fees(ctx, FB, committee).unwrap();
            let each = pending / U256::from(committee);
            assert_eq!(ctx.storage.balance(V0).unwrap(), each);
            assert_eq!(ctx.storage.balance(V1).unwrap(), U256::ZERO, "k=K weight 0");
            assert_eq!(distributed, each);
            assert_eq!(distributed + residue, pending);
        });
        let payments: Vec<_> = storage
            .get_events(REWARDS_ADDRESS)
            .iter()
            .filter_map(|log| IRewards::VoterFeeTransferred::decode_log_data(log).ok())
            .collect();
        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].voter, V0);
    }

    #[test]
    fn rounded_zero_payout_emits_no_payment() {
        let storage = run(|ctx| {
            fund(ctx, U256::from(1));
            escrow_block_fee(ctx, &block_at(10, FB), U256::from(1), &[V0]).unwrap();
            assert_eq!(
                transfer_block_fees(ctx, FB, 4).unwrap(),
                (U256::ZERO, U256::from(1))
            );
        });
        let logs = storage.get_events(REWARDS_ADDRESS);
        assert_eq!(logs.len(), 2);
        let summary = IRewards::BlockFeesTransferred::decode_log_data(&logs[1]).unwrap();
        assert_eq!(summary.transferredNative, U256::ZERO);
        assert_eq!(summary.restNative, U256::from(1));
    }

    /// A base voter (k=0) cannot be worsened by a later k>=1 re-inclusion.
    #[test]
    fn base_voter_k0_not_worsened_by_later_inclusion() {
        run(|ctx| {
            escrow_block_fee(ctx, &block_at(10, FB), U256::from(4_000u64), &[V0]).unwrap();
            record_late_vote(ctx, FB, V0, 2).unwrap(); // attempt to push to k=2
            let rewards = ctx.storage.contract::<Rewards>();
            // Stored k+1 stays 1 (k=0).
            assert_eq!(
                rewards
                    .late_voter_k_plus1
                    .get_nested(&FB)
                    .read(&V0)
                    .unwrap(),
                1
            );
            assert_eq!(
                rewards.late_voter_count.read(&FB).unwrap(),
                1,
                "no re-append"
            );
        });
    }

    /// Duplicate credit at a smaller k improves the distance (no re-append). At a
    /// larger k it is a no-op.
    #[test]
    fn smallest_k_dedup() {
        run(|ctx| {
            record_late_vote(ctx, FB, V1, 2).unwrap();
            record_late_vote(ctx, FB, V1, 1).unwrap(); // improve to k=1
            record_late_vote(ctx, FB, V1, 3).unwrap(); // no-op (larger)
            let rewards = ctx.storage.contract::<Rewards>();
            assert_eq!(
                rewards
                    .late_voter_k_plus1
                    .get_nested(&FB)
                    .read(&V1)
                    .unwrap(),
                2,
                "k+1 == 2 i.e. smallest k = 1"
            );
            assert_eq!(rewards.late_voter_count.read(&FB).unwrap(), 1);
        });
    }

    /// Settle is idempotent: a second settle pays nobody and changes nothing.
    #[test]
    fn settle_is_idempotent() {
        run(|ctx| {
            let committee = 4u64;
            let pending = U256::from(4_000u64);
            fund(ctx, pending);
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2, V3]).unwrap();
            let (d1, _) = transfer_block_fees(ctx, FB, committee).unwrap();
            let bal_after_first = ctx.storage.balance(V0).unwrap();
            let (d2, r2) = transfer_block_fees(ctx, FB, committee).unwrap();
            assert_eq!(d2, U256::ZERO);
            assert_eq!(r2, U256::ZERO);
            assert_eq!(ctx.storage.balance(V0).unwrap(), bal_after_first);
            assert_eq!(d1, pending);
        });
    }

    /// Denominator is `committee_size * w_max`.
    #[test]
    fn denominator_matches_spec() {
        assert_eq!(fixed_denominator(7), U256::from(7u64) * LATE_FINALIZE_W_MAX);
    }

    /// `seal_block_votes` settles block `N` exactly at `N+K`, looked up by number,
    /// and is a no-op before maturity and after the one settle.
    #[test]
    fn seal_block_votes_transfers_target_at_n_plus_k() {
        run(|ctx| {
            let pending = U256::from(4_000u64);
            fund(ctx, pending);
            // Escrow block N=10 (committee 4, all 4 base voters at k=0).
            escrow_block_fee(ctx, &block_at(10, FB), pending, &[V0, V1, V2, V3]).unwrap();

            // Before maturity: block 12 would settle number 9 - nothing escrowed.
            assert_eq!(
                seal_block_votes(ctx, 12, 3).unwrap(),
                (U256::ZERO, U256::ZERO)
            );

            // At N+K = 13: settles block 10's escrow.
            let (distributed, residue) = seal_block_votes(ctx, 13, 3).unwrap();
            assert_eq!(distributed, pending);
            assert_eq!(residue, U256::ZERO);
            assert!(ctx
                .storage
                .contract::<Rewards>()
                .fees_transferred
                .read(&FB)
                .unwrap());

            // Idempotent.
            assert_eq!(
                seal_block_votes(ctx, 13, 3).unwrap(),
                (U256::ZERO, U256::ZERO)
            );
        });
    }

    /// After transfer, `seal_block_votes` frees all per-window state (no state
    /// bloat). It retains only the `fees_transferred` tombstone.
    #[test]
    fn settle_frees_window_state() {
        run(|ctx| {
            let pending = U256::from(4_000u64);
            fund(ctx, pending);
            // Escrow block 10 (committee 4): V0/V1/V2 base at k=0, V3 late at k=1.
            // Non-zero view/parent_view so freeing them at settle is observable.
            escrow_block_fee(
                ctx,
                &FinalizedBlockBinding {
                    view: 9,
                    parent_view: 8,
                    ..block_at(10, FB)
                },
                pending,
                &[V0, V1, V2],
            )
            .unwrap();
            record_late_vote(ctx, FB, V3, 1).unwrap();
            assert_eq!(
                ctx.storage
                    .contract::<Rewards>()
                    .late_voter_count
                    .read(&FB)
                    .unwrap(),
                4
            );

            // Settle the matured window at N+K = 13.
            let (distributed, _residue) = seal_block_votes(ctx, 13, 3).unwrap();
            assert_eq!(distributed, pending); // all four paid (w(0)=w(1)=w_max)

            let rewards = ctx.storage.contract::<Rewards>();
            // Tombstone retained (double-settle / re-escrow guard).
            assert!(rewards.fees_transferred.read(&FB).unwrap());
            // Settlement frees all hash-keyed and number-keyed per-window state.
            assert_eq!(rewards.pending_fees.read(&FB).unwrap(), U256::ZERO);
            assert_eq!(rewards.late_voter_count.read(&FB).unwrap(), 0);
            assert_eq!(rewards.pending_fb_hash_at.read(&10).unwrap(), B256::ZERO);
            assert_eq!(rewards.pending_committee_size_at.read(&10).unwrap(), 0);
            assert_eq!(rewards.pending_epoch_at.read(&10).unwrap(), 0);
            assert_eq!(rewards.pending_view_at.read(&10).unwrap(), 0);
            assert_eq!(rewards.pending_parent_view_at.read(&10).unwrap(), 0);
            assert_eq!(
                rewards.pending_committee_set_hash_at.read(&10).unwrap(),
                B256::ZERO
            );
            let kmap = rewards.late_voter_k_plus1.get_nested(&FB);
            let at = rewards.late_voter_at.get_nested(&FB);
            for (idx, v) in [V0, V1, V2, V3].into_iter().enumerate() {
                assert_eq!(kmap.read(&v).unwrap(), 0, "k+1 freed for voter");
                assert_eq!(
                    at.read(&(idx as u32)).unwrap(),
                    Address::ZERO,
                    "voter slot freed"
                );
            }

            // Second settle is a pure no-op (number lookup zeroed + tombstone).
            assert_eq!(
                seal_block_votes(ctx, 13, 3).unwrap(),
                (U256::ZERO, U256::ZERO)
            );
        });
    }
}
