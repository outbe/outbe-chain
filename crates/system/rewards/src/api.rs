//! Public cross-module API surface for the Rewards module.
//!
//! Exposes the read-only and write entrypoints that other modules
//! (EmissionLimit, AgentReward) call as part of the daily Cycle dispatch
//! chain (`Cycle → EmissionLimit → AgentReward → Rewards`). Until this
//! refactor lands, day-boundary settle was owned by `RewardsLifecycle`
//! and triggered from `on_finalized_metadata`; with Phase 3
//! that responsibility moves out of Rewards and Rewards becomes a pure
//! storage + accounting layer that exposes the data the new orchestrator
//! needs:
//!
//! * [`read_daily_fee_sum_raw`] — locked-in raw fee total per UTC day,
//!   used by AgentReward to choose between forwarding the validator
//!   pool to Metadosis or emitting a topup.
//! * [`read_voters_for_day`] — ordered (Address, participation count)
//!   pairs for a UTC day; first-seen-on-day order is deterministic.
//! * [`add_topup_for_voters`] — credits the day's emission topup
//!   proportionally onto `pending_rewards`, mints onto REWARDS_ADDRESS
//!   to keep mint/burn parity with eventual claims, and burns the
//!   undistributed dust. Idempotent per UTC day via the dedicated
//!   `daily_topup_settled` guard.

use alloy_primitives::{Address, U256};
use outbe_gemfactory::GemTypes;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
};

use crate::runtime::day_number_since_genesis;
use crate::schema::Rewards;

/// Returns the raw fee total accumulated for the given UTC day. This is
/// the value `on_finalized_metadata` writes per finalized block via
/// `daily_fee_sum_raw[day] += validator_fee_sum`. Returns `U256::ZERO`
/// if no finalized metadata has been processed yet for `day`.
pub fn read_daily_fee_sum_raw(ctx: &BlockRuntimeContext, day: u32) -> Result<U256> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.daily_fee_sum_raw.read(&day)
}

/// Returns the deterministic, first-seen-on-day list of voter
/// participations for `day`. The vector length matches
/// `daily_voter_count[day]`; entries are ordered by the index recorded
/// in `daily_voter_at[day][i]` (i.e., the order in which the voter's
/// first finalized-block bit was observed for that day).
///
/// Each entry is `(voter_address, participation_count)` where the count
/// is the number of finalized blocks from `day` in which the voter
/// participated. Returns an empty vector if no voters have been
/// recorded for `day`.
pub fn read_voters_for_day(ctx: &BlockRuntimeContext, day: u32) -> Result<Vec<(Address, u64)>> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    let count = rewards.daily_voter_count.read(&day)?;
    let voter_at = rewards.daily_voter_at.get_nested(&day);
    let participation = rewards.daily_participation.get_nested(&day);
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count {
        let voter = voter_at.read(&i)?;
        let p = participation.read(&voter)?;
        out.push((voter, p));
    }
    Ok(out)
}

/// Distributes `topup_total` proportionally to `voters` on REWARDS_ADDRESS-backed
/// `pending_rewards` credit. Steps:
///
/// 1. Idempotency guard: if `daily_topup_settled[day]` is already set,
///    return `Ok(U256::ZERO)` without minting or crediting. This makes
///    the call safe to retry from the orchestrator without bookkeeping
///    on the caller side.
/// 2. Trivial cases: if `topup_total` is zero, `voters` is empty, or
///    the sum of participation counts is zero, mark the day settled and
///    return `Ok(U256::ZERO)` without any storage mutations on
///    `pending_rewards` or `REWARDS_ADDRESS`.
/// 3. Mint `topup_total` onto `REWARDS_ADDRESS` so the eventual claims
///    are balance-backed.
/// 4. For each voter with non-zero count, credit
///    `floor(topup_total * count / sum_count)` into
///    `pending_rewards[voter]`. Tracks the running `distributed`.
/// 5. Set `daily_topup_settled[day] = true`.
///
/// Returns the total credited (`distributed`) — useful for orchestrator
/// logging or downstream reconciliation.
///
/// Caller contract: `voters` should be the canonical ordered list from
/// [`read_voters_for_day`] for the same `day`; the api itself does not
/// re-read storage for participation counts to keep the math exactly as
/// computed by the orchestrator that selected the voter set.
pub fn add_topup_for_voters(
    ctx: &BlockRuntimeContext,
    day: u32,
    topup_total: U256,
    voters: &[(Address, u64)],
) -> Result<U256> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();

    if rewards.daily_topup_settled.read(&day)? {
        return Ok(U256::ZERO);
    }

    if topup_total.is_zero() || voters.is_empty() {
        rewards.daily_topup_settled.write(&day, true)?;
        return Ok(U256::ZERO);
    }

    let total_count: u64 = voters.iter().map(|(_, c)| *c).sum();
    if total_count == 0 {
        rewards.daily_topup_settled.write(&day, true)?;
        return Ok(U256::ZERO);
    }

    // First 21 days from genesis: validators receive Genesis gems (Qualified).
    // After that: standard Validator gems.
    let gem_type = if day_number_since_genesis(ctx, day)? < 21 {
        GemTypes::Genesis
    } else {
        GemTypes::Validator
    };

    let total_count_u256 = U256::from(total_count);
    let mut distributed = U256::ZERO;
    for (voter, count) in voters {
        if *count == 0 {
            continue;
        }
        let share_num = topup_total
            .checked_mul(U256::from(*count))
            .ok_or_else(|| PrecompileError::Revert("topup share multiply overflow".into()))?;
        let share = share_num / total_count_u256;
        if share.is_zero() {
            continue;
        }
        // 840, 840 = ISO 4217 USD for both issuance and reference currency.
        outbe_gemfactory::api::mint_gem(&ctx.storage, *voter, gem_type, share, 840, 840)?;
        distributed = distributed
            .checked_add(share)
            .ok_or_else(|| PrecompileError::Revert("topup distributed overflow".into()))?;
    }

    rewards.daily_topup_settled.write(&day, true)?;
    Ok(distributed)
}

/// Marks `day` as fully settled so `on_finalized_metadata` rejects any
/// late finalized metadata for that day. Owned by the daily Cycle
/// orchestrator: once the orchestrator has finished
/// dispatching the day's pools (validator topup, AgentReward pools,
/// Metadosis terminal credit), it calls this to flip the late-after-
/// settle guard. Idempotent.
pub fn mark_day_settled(ctx: &BlockRuntimeContext, day: u32) -> Result<()> {
    let rewards: Rewards<'_> = ctx.storage.contract::<Rewards<'_>>();
    rewards.daily_settled.write(&day, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, b256, Bytes, B256};
    use outbe_primitives::addresses::REWARDS_ADDRESS;
    use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
    use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    use crate::finalized_metadata_hook::on_finalized_metadata;
    use crate::runtime;

    const CHAIN_ID: u64 = 1;
    const GENESIS_TS: u64 = 1_704_067_200; // 2024-01-01 UTC

    const VAL_X: Address = address!("0x00000000000000000000000000000000000000A1");
    const VAL_Y: Address = address!("0x00000000000000000000000000000000000000B2");
    const VAL_Z: Address = address!("0x00000000000000000000000000000000000000C3");

    const FB_HASH_A: B256 =
        b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
    const FB_HASH_B: B256 =
        b256!("0x2222222222222222222222222222222222222222222222222222222222222222");

    fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
        BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
    }

    fn meta_with_hash(fb_hash: B256, fb_number: u64) -> CertifiedParentAccountingMetadata {
        CertifiedParentAccountingMetadata {
            finalized_block_number: fb_number,
            finalized_block_hash: fb_hash,
            finalized_epoch: 1,
            finalized_view: 1,
            parent_view: 0,
            ordered_committee: vec![],
            signer_bitmap: vec![],
            proof: Bytes::new(),
            committee_set_hash: B256::ZERO,
            vrf_material_version: 0,
            vrf_group_public_key_hash: B256::ZERO,
            proof_kind:
                outbe_primitives::consensus_metadata::ParentParticipationProof::Finalization,
            missed_proposers: vec![],
        }
    }

    fn bootstrap_genesis(ctx: &BlockRuntimeContext) {
        runtime::ensure_genesis_anchor(ctx).unwrap();
    }

    fn fund_rewards(ctx: &BlockRuntimeContext, amount: U256) {
        ctx.storage
            .increase_balance(REWARDS_ADDRESS, amount)
            .unwrap();
    }

    /// Seeds COEN/0xUSD oracle pair at `rate_1e18`. Required because
    /// `add_topup_for_voters` → `mint_gem` resolves `coen_rate` for floor
    /// price + entry_price at mint time.
    fn seed_oracle(ctx: &BlockRuntimeContext, rate_1e18: U256) {
        let mut oracle = outbe_oracle::contract::OracleContract::new(ctx.storage.clone());
        oracle.register_pair("COEN", "0xUSD").unwrap();
        oracle
            .set_exchange_rate(Address::ZERO, "COEN", "0xUSD", rate_1e18, 0, 0)
            .unwrap();
        // Register ISO 840 (USD) so mint_gem currency-validation passes.
        let pair_hash = outbe_oracle::contract::OracleContract::pair_hash("COEN", "0xUSD");
        oracle
            .settlement_iso_to_pair
            .write(&840u16, pair_hash)
            .unwrap();
        oracle.reference_currencies.push(840u16).unwrap();
    }

    fn one_e18() -> U256 {
        outbe_primitives::units::SCALE_1E18
    }

    /// Collects all gem loads owned by `voter` from the gem entity store.
    /// Returns empty Vec if voter holds no gems.
    fn voter_gem_loads(ctx: &BlockRuntimeContext, voter: Address) -> Vec<U256> {
        let gem = outbe_gem::GemContract::new(ctx.storage.clone());
        let count = gem.balance_of(voter).unwrap();
        (0..count)
            .map(|i| {
                let gem_id = gem.token_of_owner_by_index(voter, i).unwrap();
                outbe_gem::api::get_gem(&ctx.storage, gem_id)
                    .unwrap()
                    .unwrap()
                    .gem_load
            })
            .collect()
    }

    #[test]
    fn read_daily_fee_sum_raw_returns_zero_when_unrecorded() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
            bootstrap_genesis(&ctx);

            assert_eq!(read_daily_fee_sum_raw(&ctx, 20240101).unwrap(), U256::ZERO);
        });
    }

    #[test]
    fn read_daily_fee_sum_raw_round_trips_after_finalized_metadata() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
            bootstrap_genesis(&ctx);
            fund_rewards(&ctx, U256::from(300u64));

            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_A, 1),
                U256::from(101u64),
                GENESIS_TS,
                &[VAL_X, VAL_Y],
            )
            .unwrap();
            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_B, 2),
                U256::from(199u64),
                GENESIS_TS,
                &[VAL_X, VAL_Y],
            )
            .unwrap();

            // 101 + 199 = 300 raw.
            assert_eq!(
                read_daily_fee_sum_raw(&ctx, 20240101).unwrap(),
                U256::from(300u64)
            );
            // Untouched day stays zero.
            assert_eq!(read_daily_fee_sum_raw(&ctx, 20240102).unwrap(), U256::ZERO);
        });
    }

    #[test]
    fn read_voters_for_day_orders_by_first_seen() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
            bootstrap_genesis(&ctx);
            fund_rewards(&ctx, U256::from(400u64));

            // FB_HASH_A: Y first, then X (first-seen-on-day = Y, X)
            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_A, 1),
                U256::from(100u64),
                GENESIS_TS,
                &[VAL_Y, VAL_X],
            )
            .unwrap();
            // FB_HASH_B (same day): Z is new, X already seen
            on_finalized_metadata(
                &ctx,
                &meta_with_hash(FB_HASH_B, 2),
                U256::from(100u64),
                GENESIS_TS,
                &[VAL_X, VAL_Z],
            )
            .unwrap();

            let voters = read_voters_for_day(&ctx, 20240101).unwrap();
            // First-seen order: Y (block A), X (block A), Z (block B).
            assert_eq!(voters.len(), 3);
            assert_eq!(voters[0].0, VAL_Y);
            assert_eq!(voters[0].1, 1);
            assert_eq!(voters[1].0, VAL_X);
            assert_eq!(voters[1].1, 2); // X participated in both A and B
            assert_eq!(voters[2].0, VAL_Z);
            assert_eq!(voters[2].1, 1);
        });
    }

    #[test]
    fn read_voters_for_day_empty_when_no_metadata() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
            bootstrap_genesis(&ctx);

            let voters = read_voters_for_day(&ctx, 20240101).unwrap();
            assert!(voters.is_empty());
        });
    }

    #[test]
    fn add_topup_for_voters_mints_gems_proportionally() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
            bootstrap_genesis(&ctx);
            seed_oracle(&ctx, U256::from(2u64) * one_e18());

            // counts 1 + 3 = 4; topup 400 → VAL_X 100, VAL_Y 300.
            let voters = vec![(VAL_X, 1u64), (VAL_Y, 3u64)];
            let distributed =
                add_topup_for_voters(&ctx, 20240101, U256::from(400u64), &voters).unwrap();
            assert_eq!(distributed, U256::from(400u64));

            assert_eq!(voter_gem_loads(&ctx, VAL_X), vec![U256::from(100u64)]);
            assert_eq!(voter_gem_loads(&ctx, VAL_Y), vec![U256::from(300u64)]);

            let rewards = ctx.storage.contract::<Rewards>();
            assert!(rewards.daily_topup_settled.read(&20240101).unwrap());
        });
    }

    #[test]
    fn add_topup_for_voters_picks_genesis_vs_validator_by_window() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        storage.enter(|handle| {
            let ctx = BlockRuntimeContext::new(block_ctx(1, GENESIS_TS + 60), handle);
            bootstrap_genesis(&ctx);
            seed_oracle(&ctx, U256::from(2u64) * one_e18());

            // Day 0: within the 21-day genesis window → Genesis gem (Qualified).
            let voters = vec![(VAL_X, 1u64)];
            add_topup_for_voters(&ctx, 20240101, U256::from(50u64), &voters).unwrap();

            // Day 31 (since 2024-01-01): past the window → Validator gem (Issued).
            let voters_b = vec![(VAL_Y, 1u64)];
            add_topup_for_voters(&ctx, 20240201, U256::from(70u64), &voters_b).unwrap();

            let gem = outbe_gem::GemContract::new(ctx.storage.clone());

            assert_eq!(gem.balance_of(VAL_X).unwrap(), 1);
            let x_gem_id = gem.token_of_owner_by_index(VAL_X, 0).unwrap();
            let x_item = outbe_gem::api::get_gem(&ctx.storage, x_gem_id)
                .unwrap()
                .unwrap();
            assert_eq!(x_item.gem_type, GemTypes::Genesis as u8);
            assert_eq!(
                x_item.state,
                outbe_gem::GemState::Qualified as u8,
                "Genesis gem is born Qualified"
            );

            assert_eq!(gem.balance_of(VAL_Y).unwrap(), 1);
            let y_gem_id = gem.token_of_owner_by_index(VAL_Y, 0).unwrap();
            let y_item = outbe_gem::api::get_gem(&ctx.storage, y_gem_id)
                .unwrap()
                .unwrap();
            assert_eq!(y_item.gem_type, GemTypes::Validator as u8);
            assert_eq!(
                y_item.state,
                outbe_gem::GemState::Issued as u8,
                "Post-genesis Validator gem is born Issued"
            );
        });
    }
}
