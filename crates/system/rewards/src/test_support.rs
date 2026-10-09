//! Fixtures shared by the Rewards unit tests.

use alloy_primitives::{address, b256, Address, Bytes, B256, U256};
use outbe_primitives::addresses::REWARDS_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;

use crate::finalized_metadata_hook::on_finalized_metadata;
use crate::runtime;

pub(crate) const CHAIN_ID: u64 = 1;
/// Genesis at midnight UTC of 2024-01-01.
pub(crate) const GENESIS_TS: u64 = 1_704_067_200;
pub(crate) const SECONDS_PER_DAY: u64 = 86_400;

pub(crate) const FB_HASH_A: B256 =
    b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
pub(crate) const FB_HASH_B: B256 =
    b256!("0x2222222222222222222222222222222222222222222222222222222222222222");
pub(crate) const VAL_X: Address = address!("0x00000000000000000000000000000000000000A1");
pub(crate) const VAL_Y: Address = address!("0x00000000000000000000000000000000000000B2");
pub(crate) const VAL_Z: Address = address!("0x00000000000000000000000000000000000000C3");

pub(crate) fn block_ctx(block_number: u64, timestamp: u64) -> BlockContext {
    BlockContext::new(block_number, timestamp, CHAIN_ID, Address::ZERO, Vec::new())
}

/// Finalized-block metadata of block `fb_number` with hash `fb_hash`, an empty
/// committee and no signers.
pub(crate) fn meta_with_hash(fb_hash: B256, fb_number: u64) -> CertifiedParentAccountingMetadata {
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
        proof_kind: ParentParticipationProof::Finalization,
        missed_proposers: vec![],
    }
}

/// Locks in `genesis_utc_day` so that `day_number_since_genesis` works.
pub(crate) fn bootstrap_genesis(ctx: &BlockRuntimeContext) {
    runtime::ensure_genesis_anchor(ctx).unwrap();
}

/// Credits `amount` to `REWARDS_ADDRESS` so that transfers from it succeed.
pub(crate) fn fund_rewards(ctx: &BlockRuntimeContext, amount: U256) {
    ctx.storage
        .increase_balance(REWARDS_ADDRESS, amount)
        .unwrap();
}

/// Runs `f` on fresh storage in block `block_number` at `timestamp`.
pub(crate) fn with_block(
    block_number: u64,
    timestamp: u64,
    f: impl FnOnce(BlockRuntimeContext<'_>),
) {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.enter(|handle| {
        f(BlockRuntimeContext::new(
            block_ctx(block_number, timestamp),
            handle,
        ))
    });
}

/// Runs `f` on fresh storage in block 1, one minute after genesis, after the
/// genesis anchor.
pub(crate) fn with_genesis_block(f: impl FnOnce(BlockRuntimeContext<'_>)) {
    with_block(1, GENESIS_TS + 60, |ctx| {
        bootstrap_genesis(&ctx);
        f(ctx);
    });
}

/// [`with_genesis_block`] with `amount` credited to `REWARDS_ADDRESS` first.
pub(crate) fn with_funded_genesis_block(amount: u64, f: impl FnOnce(BlockRuntimeContext<'_>)) {
    with_genesis_block(|ctx| {
        fund_rewards(&ctx, U256::from(amount));
        f(ctx);
    });
}

/// Runs the finalized-metadata hook for `meta` with `fees` and the base
/// `voters`, as a parent block finalized at genesis.
pub(crate) fn record_genesis_day_parent(
    ctx: &BlockRuntimeContext,
    meta: &CertifiedParentAccountingMetadata,
    fees: u64,
    voters: &[Address],
) {
    on_finalized_metadata(ctx, meta, U256::from(fees), GENESIS_TS, voters).unwrap();
}
