use alloy_primitives::{Address, B256, U256};
use outbe_macros::contract;
use outbe_primitives::addresses::REWARDS_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot};

/// EVM storage layout for the Rewards precompile.
///
/// The layout tracks:
/// - the genesis UTC-day anchor of the chain.
/// - the per-finalized-block and per-day accumulators for the idempotent fee-distribution path
///   and the day-boundary settle formula.
///
/// Per-block fees are escrowed (`pending_fees`) and settled at `N+K` over the
/// inclusion-window voter set. The daily emission top-up is credited to the
/// validator pool of AgentReward, weighted by [`crate::api::read_voters_for_day`].
///
/// Storage slots:
///   0:  participation_counted_for_block    - mapping(B256 => mapping(address => bool))
///   1:  daily_fee_sum_raw                  - mapping(uint32 => uint256)
///   2:  daily_participation                - mapping(uint32 => mapping(address => uint64))
///   3:  daily_voter_count                  - mapping(uint32 => uint32)
///   4:  daily_voter_at                     - mapping(uint32 => mapping(uint32 => address))
///   5:  block_metadata_counted             - mapping(B256 => bool)
///   6:  metadata_fingerprint_for_block     - mapping(B256 => B256)
///   7:  pending_fees                       - mapping(B256 => uint256)
///   8:  fee_settled                        - mapping(B256 => bool)
///   9:  late_voter_k_plus1                 - mapping(B256 => mapping(address => uint8))
///  10:  late_voter_count                   - mapping(B256 => uint32)
///  11:  late_voter_at                      - mapping(B256 => mapping(uint32 => address))
///  12:  pending_fb_hash_at                 - mapping(uint64 => B256)    (number -> fb_hash)
///  13:  pending_committee_size_at          - mapping(uint64 => uint32)
///  14:  pending_epoch_at                   - mapping(uint64 => uint64)
///  15:  pending_committee_set_hash_at      - mapping(uint64 => B256)
///  16:  pending_view_at                    - mapping(uint64 => uint64)
///  17:  pending_parent_view_at             - mapping(uint64 => uint64)
///  18:  block_guard_ring                   - mapping(uint64 => B256) (prune ring of fb_hash)
///  19:  block_guard_ring_seq               - uint64 (ring write cursor)
///  20:  pending_reward_day                 - mapping(B256 => uint32)
///  21:  daily_last_window_close            - mapping(uint32 => uint64)
///  22:  late_residue_dust_native           - uint256
///  23:  daily_participation_closed         - mapping(uint32 => bool)
#[contract(addr = REWARDS_ADDRESS)]
pub struct Rewards {
    /// Per-(finalized_block_hash, voter) guard: voter participation has
    /// already been counted for this finalized block in the day's
    /// emission top-up share. This guard is separate from the fee guard for
    /// audit clarity.
    pub participation_counted_for_block: Mapping<B256, Mapping<Address, bool>>,

    /// Per-day raw fee sum from finalized metadata (`fees_raw`). Source
    /// of truth for the cap-vs-fees formula at settle time. The emission cap is
    /// six-decimal protocol value while this sum stays in native COEN minor units
    /// (wei; 10^18 per COEN). One protocol unit equals 10^12 native units:
    ///   `topup = floor((cap * 1e12).saturating_sub(daily_fee_sum_raw) / 1e12)`
    pub daily_fee_sum_raw: Mapping<u32, U256>,

    /// Per-(utc_day, voter) participation count for the day. Used to
    /// proportionally distribute the day's emission top-up.
    pub daily_participation: Mapping<u32, Mapping<Address, u64>>,

    /// Per-day count of distinct voters seen. It is the upper bound for index
    /// iteration over `daily_voter_at`.
    pub daily_voter_count: Mapping<u32, u32>,

    /// Per-(utc_day, index) voter address. The index is first-seen-order
    /// of the voter for that day. `participation_counted_for_block` maintains
    /// replay safety. It ensures that the append happens exactly once per
    /// `(finalized_block_hash, voter)`, regardless of replay order.
    pub daily_voter_at: Mapping<u32, Mapping<u32, Address>>,

    /// Per-finalized-block guard: cap/fees-raw from this block already
    /// accumulated into `daily_fee_sum_raw`.
    pub block_metadata_counted: Mapping<B256, bool>,

    /// Per-finalized-block fingerprint guard. The fingerprint is
    /// `keccak256("OUTBE_METADATA_FINGERPRINT_V3" || canonical-encoded
    /// metadata economic fields)`. The same fingerprint twice for the same
    /// `fb_hash` is a replay no-op. A different fingerprint returns `Revert`.
    /// CertifiedParentAccounting rejects the block on that revert.
    pub metadata_fingerprint_for_block: Mapping<B256, B256>,

    // -- per-block fee escrow + inclusion-window credits ----------
    /// Escrowed fees in native COEN minor units (wei; 10^18 per COEN), keyed by
    /// `fb_hash`. One six-decimal protocol unit equals 10^12 native units. Recorded instead of
    /// the eager per-voter transfer. Settled at `N+K` across the credited voter
    /// set with a decay-weighted, fixed-denominator payout. Whole protocol units
    /// of residue plus carried dust burn; the sub-unit remainder stays as dust.
    pub pending_fees: Mapping<B256, U256>,

    /// Per-finalized-block guard: the escrowed fee has been settled exactly once.
    pub fee_settled: Mapping<B256, bool>,

    /// Per-(fb_hash, voter) smallest inclusion distance, stored as `k + 1`
    /// (`0` = not yet credited). Thus the base 2f+1 seeded at `k=0` records `1`, and
    /// a later re-inclusion at `k>=1` is a no-op.
    pub late_voter_k_plus1: Mapping<B256, Mapping<Address, u8>>,

    /// Per-fb_hash count of distinct credited voters. It bounds the index iteration
    /// over `late_voter_at` at settle time.
    pub late_voter_count: Mapping<B256, u32>,

    /// Per-(fb_hash, index) credited voter address, in first-credit order, so the
    /// settlement can enumerate the voter set deterministically.
    pub late_voter_at: Mapping<B256, Mapping<u32, Address>>,

    /// Settle-trigger lookup: finalized block number -> its `fb_hash`, so the
    /// `LateFinalizeCredits` phase at block `N+K` can settle block `N`'s escrow
    /// by number (the mandatory window-close side effect). `B256::ZERO` = absent.
    pub pending_fb_hash_at: Mapping<u64, B256>,

    /// Finalized block number -> committee size at its epoch. This is the fixed
    /// denominator basis (`D = committee_size * w_max`) for its settlement.
    pub pending_committee_size_at: Mapping<u64, u32>,

    /// Finalized block number -> its canonical consensus epoch.
    /// The `LateFinalizeCredits` phase verifies a credit's
    /// proposer-supplied `epoch`/`committee_set_hash`/`fb_hash` against the
    /// canonical binding escrowed here. Thus `fb_number` cannot be spoofed to
    /// inflate the decay weight `k` or to reference a wrong committee.
    pub pending_epoch_at: Mapping<u64, u64>,

    /// Finalized block number -> its canonical `committee_set_hash`, for
    /// authentication. See `pending_epoch_at`.
    pub pending_committee_set_hash_at: Mapping<u64, B256>,

    /// Maps each finalized block number to its canonical consensus `view`.
    /// Together with `pending_parent_view_at`, this map stores the full signed
    /// binding `{epoch, view, parent_view, fb_hash}` at the body.
    /// This binding prevents a proposer from recording a credit for a noncanonical
    /// view of the same `fb_hash` (cross-view equivocation).
    /// Pre-execution BLS verification only checks the credit's view against its signatures.
    /// These maps bind the credit to the finalized certificate.
    ///
    /// Consensus checks the binding against the signed proposal before it processes
    /// certified parent accounting (CPA) metadata.
    /// EVM `LateFinalizeCredits` checks the stored binding.
    /// The V3 economic replay fingerprint does not hash `parent_view`.
    /// It does not authenticate certificates.
    pub pending_view_at: Mapping<u64, u64>,

    /// Finalized block number -> its canonical `parent_view`, for
    /// authentication. See `pending_view_at`.
    pub pending_parent_view_at: Mapping<u64, u64>,

    // -- prune ring for the per-finalized-block guard maps ----------
    /// Bounds the per-`fb_hash` guard maps (`block_metadata_counted`,
    /// `metadata_fingerprint_for_block`, `fee_settled`) to the last [`BLOCK_GUARD_RETAIN`](crate::finalized_metadata_hook::BLOCK_GUARD_RETAIN)
    /// finalized blocks. `on_finalized_metadata` records each finalized `fb_hash`
    /// here and clears the three guards of the block evicted `BLOCK_GUARD_RETAIN`
    /// records ago. Retention is far larger than the K-block late-finalize
    /// window. Thus no guard is dropped while its block can still be replayed
    /// (re-counted) or settled. The nested `participation_counted_for_block` map
    /// is freed separately at settlement (`settle_window`), where the credited
    /// voter set is known. `B256::ZERO` = empty ring slot.
    pub block_guard_ring: Mapping<u64, B256>,

    /// Monotonic write cursor for `block_guard_ring`. The live slot index is
    /// `block_guard_ring_seq % BLOCK_GUARD_RETAIN`.
    pub block_guard_ring_seq: Slot<u64>,

    /// Canonical UTC day of an open finalized-block window. Cleared with escrow.
    pub pending_reward_day: Mapping<B256, u32>,

    /// Last inclusion-window close height for each day's finalized blocks.
    /// LateFinalizeCredits runs before CycleTick in the same block.
    /// Day settlement still requires a height above this close.
    pub daily_last_window_close: Mapping<u32, u64>,

    /// Late-settlement residue in native COEN minor units (wei; 10^18 per COEN),
    /// carried to the next settlement. Below one six-decimal protocol unit (10^12 native units).
    pub late_residue_dust_native: Slot<U256>,

    /// The day's validator pool has been credited; its participation is final.
    pub daily_participation_closed: Mapping<u32, bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn storage_slot_layout_is_pinned() {
        let mut provider = HashMapStorageProvider::new(1);
        let c = Rewards::new(StorageHandle::new(&mut provider));

        assert_eq!(c.participation_counted_for_block.base_slot(), U256::from(0));
        assert_eq!(c.daily_fee_sum_raw.base_slot(), U256::from(1));
        assert_eq!(c.daily_participation.base_slot(), U256::from(2));
        assert_eq!(c.daily_voter_count.base_slot(), U256::from(3));
        assert_eq!(c.daily_voter_at.base_slot(), U256::from(4));
        assert_eq!(c.block_metadata_counted.base_slot(), U256::from(5));
        assert_eq!(c.metadata_fingerprint_for_block.base_slot(), U256::from(6));
        assert_eq!(c.pending_fees.base_slot(), U256::from(7));
        assert_eq!(c.fee_settled.base_slot(), U256::from(8));
        assert_eq!(c.late_voter_k_plus1.base_slot(), U256::from(9));
        assert_eq!(c.late_voter_count.base_slot(), U256::from(10));
        assert_eq!(c.late_voter_at.base_slot(), U256::from(11));
        assert_eq!(c.pending_fb_hash_at.base_slot(), U256::from(12));
        assert_eq!(c.pending_committee_size_at.base_slot(), U256::from(13));
        assert_eq!(c.pending_epoch_at.base_slot(), U256::from(14));
        assert_eq!(c.pending_committee_set_hash_at.base_slot(), U256::from(15));
        assert_eq!(c.pending_view_at.base_slot(), U256::from(16));
        assert_eq!(c.pending_parent_view_at.base_slot(), U256::from(17));
        assert_eq!(c.block_guard_ring.base_slot(), U256::from(18));
        assert_eq!(c.block_guard_ring_seq.slot(), U256::from(19));
        assert_eq!(c.pending_reward_day.base_slot(), U256::from(20));
        assert_eq!(c.daily_last_window_close.base_slot(), U256::from(21));
        assert_eq!(c.late_residue_dust_native.slot(), U256::from(22));
        assert_eq!(c.daily_participation_closed.base_slot(), U256::from(23));
    }
}
