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
///   0:  genesis_utc_day                    - uint32 (yyyymmdd of block 0; 0 = uninit)
///   1:  participation_counted_for_block    - mapping(B256 => mapping(address => bool))
///   2:  daily_fee_sum_raw                  - mapping(uint32 => uint256)
///   3:  daily_participation                - mapping(uint32 => mapping(address => uint64))
///   4:  daily_voter_count                  - mapping(uint32 => uint32)
///   5:  daily_voter_at                     - mapping(uint32 => mapping(uint32 => address))
///   6:  daily_settled                      - mapping(uint32 => bool)
///   7:  block_metadata_counted             - mapping(B256 => bool)
///   8:  metadata_fingerprint_for_block     - mapping(B256 => B256)
///   9:  pending_fees                       - mapping(B256 => uint256)
///  10:  fee_settled                        - mapping(B256 => bool)
///  11:  late_voter_k_plus1                 - mapping(B256 => mapping(address => uint8))
///  12:  late_voter_count                   - mapping(B256 => uint32)
///  13:  late_voter_at                      - mapping(B256 => mapping(uint32 => address))
///  14:  pending_fb_hash_at                 - mapping(uint64 => B256)    (number -> fb_hash)
///  15:  pending_committee_size_at          - mapping(uint64 => uint32)
///  16:  pending_epoch_at                   - mapping(uint64 => uint64)
///  17:  pending_committee_set_hash_at      - mapping(uint64 => B256)
///  18:  pending_view_at                    - mapping(uint64 => uint64)
///  19:  pending_parent_view_at             - mapping(uint64 => uint64)
///  20:  block_guard_ring                   - mapping(uint64 => B256) (prune ring of fb_hash)
///  21:  block_guard_ring_seq               - uint64 (ring write cursor)
///  22:  pending_reward_day                 - mapping(B256 => uint32)
///  23:  daily_last_window_close            - mapping(uint32 => uint64)
///  24:  late_residue_dust_native           - uint256
///  25:  daily_participation_closed         - mapping(uint32 => bool)
#[contract(addr = REWARDS_ADDRESS)]
pub struct Rewards {
    /// UTC day of block 0 (yyyymmdd). 0 means uninitialized. `RewardsLifecycle::begin_block`
    /// writes it exactly once at its first invocation, which is block 0 in any chain shipping
    /// this refactor. After initialization the value is immutable.
    /// `day_emission_limit(day_number_since_genesis(...))` from
    /// `outbe_emissionlimit::day_emission` uses it.
    ///
    /// Tamper-resistance: a node that boots with a different `genesis.json` timestamp locks in
    /// a different `genesis_utc_day` here. Then all subsequent day-settle math diverges from
    /// the state root of the quorum, and the node falls out of consensus on the first settle.
    /// Reth's startup hash check is a complementary defense, not the only one.
    pub genesis_utc_day: Slot<u32>,

    /// Per-(finalized_block_hash, voter) guard: voter participation has
    /// already been counted for this finalized block in the day's
    /// emission top-up share. This guard is separate from the fee guard for
    /// audit clarity.
    pub participation_counted_for_block: Mapping<B256, Mapping<Address, bool>>,

    /// Per-day raw fee sum from finalized metadata (`fees_raw`). Source
    /// of truth for the cap-vs-fees formula at settle time. The emission cap is
    /// six-decimal protocol value while this sum stays raw native COEN:
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

    /// Per-day marker that Cycle settled the day once.
    /// Settlement is gated by `day_participation_complete`.
    /// `on_finalized_metadata` does not reject late metadata for a settled day.
    pub daily_settled: Mapping<u32, bool>,

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
    /// Escrowed fees of a finalized block (key `fb_hash`), recorded instead of
    /// the eager per-voter transfer. Settled at `N+K` across the credited voter
    /// set with a decay-weighted, fixed-denominator payout. Residue burns.
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

    /// Finalized block number -> its canonical consensus `view`, for
    /// authentication. Together with `pending_parent_view_at`, this pins the FULL
    /// signed binding `{epoch, view, parent_view, fb_hash}` at the body. Thus a
    /// proposer cannot record a credit whose aggregate is over a non-canonical
    /// view of the same `fb_hash` (cross-view equivocation). The pre-exec BLS
    /// verify only ties the credit's view to its signatures. This map ties it to the
    /// finalized certificate.
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

    /// Native late-settlement residue below one protocol unit, carried to the next settlement.
    pub late_residue_dust_native: Slot<U256>,

    /// The day's validator pool has been credited; its participation is final.
    pub daily_participation_closed: Mapping<u32, bool>,
}
