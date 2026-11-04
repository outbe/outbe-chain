//! Daily validator rewards include canonical late voters, including across UTC midnight.
use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
    storage::hashmap::HashMapStorageProvider,
};
use outbe_rewards::{api, block_fees, finalized_metadata_hook::record_votes_and_fees};

const DAY: u32 = 20240101;
const MIDNIGHT: u64 = 1_704_153_600;
const HASH: B256 = B256::repeat_byte(0x42);
const VOTERS: [Address; 4] = [
    Address::repeat_byte(1),
    Address::repeat_byte(2),
    Address::repeat_byte(3),
    Address::repeat_byte(4),
];

fn block(height: u64) -> BlockContext {
    BlockContext {
        block_number: height,
        timestamp: MIDNIGHT + height - 11,
        chain_id: 1,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: (MIDNIGHT + height - 11).max(1),
        proposer: Address::ZERO,
        validators: vec![],
    }
}

fn seed(ctx: &BlockRuntimeContext) {
    let metadata = CertifiedParentAccountingMetadata {
        finalized_block_number: 10,
        finalized_block_hash: HASH,
        finalized_epoch: 0,
        finalized_view: 10,
        parent_view: 9,
        ordered_committee: VOTERS.to_vec(),
        signer_bitmap: vec![7],
        proof: Bytes::new(),
        committee_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key_hash: B256::ZERO,
        proof_kind: ParentParticipationProof::Finalization,
        missed_proposers: vec![],
    };
    // This seam consumes already-verified CPA. Cryptographic checks live in EVM.
    record_votes_and_fees(ctx, &metadata, U256::ZERO, MIDNIGHT - 1, &VOTERS[..3]).unwrap();
}

#[test]
fn midnight_late_vote_survives_reentry_and_counts_once() {
    let mut storage = HashMapStorageProvider::new(1);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block(11), handle);
        seed(&ctx);
        assert!(!api::is_voting_complete(&ctx, DAY).unwrap());
        block_fees::record_late_vote(&ctx, HASH, VOTERS[3], 1).unwrap();
    });
    // Re-enter the same storage provider with new context handles. Day binding
    // and dedup live in contract storage, not context-local bookkeeping. This
    // is an in-memory provider test, not a disk/process restart test.
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block(13), handle);
        block_fees::record_late_vote(&ctx, HASH, VOTERS[3], 3).unwrap();
        block_fees::record_late_vote(&ctx, HASH, VOTERS[0], 2).unwrap();
        assert_eq!(
            api::get_day_voters(&ctx, DAY).unwrap(),
            VOTERS.map(|v| (v, 1)).to_vec()
        );
        assert!(api::get_day_voters(&ctx, DAY + 1).unwrap().is_empty());
        assert!(!api::is_voting_complete(&ctx, DAY).unwrap());
        block_fees::seal_block_votes(&ctx, 13, 3).unwrap();
        assert_eq!(
            ctx.storage
                .contract::<outbe_rewards::schema::Rewards>()
                .pending_reward_day
                .read(&HASH)
                .unwrap(),
            0
        );
        // The guard was pruned, but the settled-window tombstone rejects replay.
        block_fees::record_late_vote(&ctx, HASH, VOTERS[3], 1).unwrap();
    });
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block(14), handle);
        assert!(api::is_voting_complete(&ctx, DAY).unwrap());
        // Every voter weighs the same in the day's validator pool.
        assert_eq!(
            api::get_day_voters(&ctx, DAY).unwrap(),
            VOTERS.map(|v| (v, 1)).to_vec()
        );
    });
}

#[test]
fn last_admissible_slot_counts_for_the_reward_even_when_fee_weight_is_zero() {
    let mut storage = HashMapStorageProvider::new(1);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block(13), handle);
        seed(&ctx);
        block_fees::record_late_vote(&ctx, HASH, VOTERS[3], 3).unwrap();
        assert_eq!(
            api::get_day_voters(&ctx, DAY).unwrap(),
            VOTERS.map(|v| (v, 1)).to_vec()
        );
        assert_eq!(outbe_rewards::constants::decay_weight(3), U256::ZERO);
    });
}

#[test]
fn unknown_or_out_of_window_credit_cannot_add_reward_participation() {
    let mut storage = HashMapStorageProvider::new(1);
    storage.enter(|handle| {
        let ctx = BlockRuntimeContext::new(block(11), handle);
        seed(&ctx);
        let before = api::get_day_voters(&ctx, DAY).unwrap();
        assert!(block_fees::record_late_vote(&ctx, B256::repeat_byte(0x99), VOTERS[3], 1).is_err());
        assert!(block_fees::record_late_vote(&ctx, HASH, VOTERS[3], 4).is_err());
        assert_eq!(api::get_day_voters(&ctx, DAY).unwrap(), before);
    });
}
