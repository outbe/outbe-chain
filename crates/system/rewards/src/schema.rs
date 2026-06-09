use alloy_primitives::{Address, B256, U256};
use outbe_macros::contract;
use outbe_primitives::addresses::REWARDS_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot};

/// EVM storage layout for the Rewards precompile.
///
/// Tracks claimable emission top-up balances, the chain's genesis UTC-day
/// anchor, and the per-finalized-block + per-day accumulators used by the
/// idempotent fee-distribution path and day-boundary settle formula.
///
/// Per-block fees are escrowed (`pending_fees`) and settled at `N+K` over
/// the inclusion-window voter set; emission top-ups land in `pending_rewards`
/// once per UTC day at settle time and are claimed via `claimRewards`.
///
/// Storage slots:
///   0:  pending_rewards                    — mapping(address => uint256)
///   1:  genesis_utc_day                    — uint32 (yyyymmdd of block 0; 0 = uninit)
///   2:  participation_counted_for_block    — mapping(B256 => mapping(address => bool))
///   3:  daily_fee_sum_raw                  — mapping(uint32 => uint256)
///   4:  daily_fees_paid                    — mapping(uint32 => uint256)
///   5:  daily_fee_dust                     — mapping(uint32 => uint256)
///   6:  daily_participation                — mapping(uint32 => mapping(address => uint64))
///   7:  daily_total_participation          — mapping(uint32 => uint64)
///   8:  daily_voter_count                  — mapping(uint32 => uint32)
///   9:  daily_voter_at                     — mapping(uint32 => mapping(uint32 => address))
///  10:  daily_settled                      — mapping(uint32 => bool)
///  11:  max_observed_finalized_day         — uint32 (yyyymmdd; 0 = uninit)
///  12:  last_settled_utc_day               — uint32 (yyyymmdd; 0 = uninit)
///  13:  block_metadata_counted             — mapping(B256 => bool)
///  14:  metadata_fingerprint_for_block     — mapping(B256 => B256)
///  15:  fee_dust_counted_for_block         — mapping(B256 => bool)
///  16:  daily_topup_settled                — mapping(uint32 => bool)
///  17:  pending_fees                       — mapping(B256 => uint256)
///  18:  fee_settled                        — mapping(B256 => bool)
///  19:  late_voter_k_plus1                 — mapping(B256 => mapping(address => uint8))
///  20:  late_voter_count                   — mapping(B256 => uint32)
///  21:  late_voter_at                      — mapping(B256 => mapping(uint32 => address))
///  22:  pending_fb_hash_at                 — mapping(uint64 => B256)    (number -> fb_hash)
///  23:  pending_committee_size_at          — mapping(uint64 => uint32)
///  24:  pending_epoch_at                   — mapping(uint64 => uint64)
///  25:  pending_committee_set_hash_at      — mapping(uint64 => B256)
///  26:  pending_view_at                    — mapping(uint64 => uint64)
///  27:  pending_parent_view_at             — mapping(uint64 => uint64)
#[contract(addr = REWARDS_ADDRESS)]
pub struct Rewards {
    /// Accumulated claimable emission top-up per validator address.
    /// Per-block fees are NOT credited here — they are escrowed in
    /// `pending_fees` and settled at `N+K` (vote.md). Only the daily
    /// emission top-up share lands here, claimable via `claimRewards`.
    pub pending_rewards: Mapping<Address, U256>,

    /// UTC day of block 0 (yyyymmdd). 0 means uninitialized; written
    /// exactly once at the first invocation of `RewardsLifecycle::begin_block`,
    /// which is block 0 in any chain shipping this refactor. After
    /// initialization the value is immutable and used by
    /// `day_emission_limit(day_number_since_genesis(...))` from
    /// `outbe_emissionlimit::daily_emission`.
    ///
    /// Tamper-resistance: a node that boots with a different
    /// `genesis.json` timestamp will lock in a different `genesis_utc_day`
    /// here, causing all subsequent day-settle math to diverge from the
    /// quorum's state root → fall out of consensus on the first settle.
    /// Reth's startup hash check is a complementary defense, not the only
    /// one.
    pub genesis_utc_day: Slot<u32>,

    /// Per-(finalized_block_hash, voter) guard: voter participation has
    /// already been counted for this finalized block in the day's
    /// emission top-up share. Kept separate from the fee guard for
    /// audit clarity.
    pub participation_counted_for_block: Mapping<B256, Mapping<Address, bool>>,

    /// Per-day raw fee sum from finalized metadata (`fees_raw`). Source
    /// of truth for the cap-vs-fees formula at settle time:
    ///   `topup           = cap.saturating_sub(daily_fee_sum_raw)`
    ///   `fee_against_cap = min(daily_fee_sum_raw, cap)`
    /// Equals `daily_fees_paid + daily_fee_dust` (invariant).
    pub daily_fee_sum_raw: Mapping<u32, U256>,

    /// Per-day total fees actually transferred to voters today, computed
    /// as `floor(fees_raw / voters_count) * voters_count`.
    pub daily_fees_paid: Mapping<u32, U256>,

    /// Per-day fee dust accumulated on `REWARDS_ADDRESS` (the residue
    /// from per-block split). Burned at settle and credited to Metadosis
    /// terminal limit.
    pub daily_fee_dust: Mapping<u32, U256>,

    /// Per-(utc_day, voter) participation count for the day. Used to
    /// proportionally distribute the day's emission top-up.
    pub daily_participation: Mapping<u32, Mapping<Address, u64>>,

    /// Per-day sum of `daily_participation` values for O(1) access at
    /// settle time. Maintained alongside `daily_participation` to avoid
    /// iterating the inner mapping (Mapping has no native iteration).
    pub daily_total_participation: Mapping<u32, u64>,

    /// Per-day count of distinct voters seen — upper bound for index
    /// iteration over `daily_voter_at`.
    pub daily_voter_count: Mapping<u32, u32>,

    /// Per-(utc_day, index) voter address. The index is first-seen-order
    /// of the voter for that day. Replay safety is maintained by
    /// `participation_counted_for_block`, which ensures append happens
    /// exactly once per `(finalized_block_hash, voter)` regardless of
    /// replay order.
    pub daily_voter_at: Mapping<u32, Mapping<u32, Address>>,

    /// Per-day guard that the day has been settled exactly once.
    /// Settle is gated on `max_observed_finalized_day > D` so late
    /// metadata for an already-settled day is rejected as fatal.
    pub daily_settled: Mapping<u32, bool>,

    /// Highest UTC day observed among processed finalized blocks
    /// (yyyymmdd; 0 = uninit). Used to gate the daily Cycle handler in
    /// `outbe-cycle`: a day D is settle-eligible only when at least one
    /// finalized block
    /// from a strictly later UTC day has been observed, so we are
    /// certain no further metadata for D will arrive.
    pub max_observed_finalized_day: Slot<u32>,

    /// Last UTC day successfully settled by `RewardsLifecycle`
    /// (yyyymmdd; 0 = uninit). Initialized lazily on first observed
    /// finalized day to `previous_date_key(fb_day)` so the first
    /// eligible day is exactly the first observed finalized day.
    pub last_settled_utc_day: Slot<u32>,

    /// Per-finalized-block guard: cap/fees-raw from this block already
    /// accumulated into `daily_fee_sum_raw`. Independent of fee-dust
    /// guard so dust accumulation has its own short-circuit semantics.
    pub block_metadata_counted: Mapping<B256, bool>,

    /// Per-finalized-block fingerprint guard. The fingerprint is
    /// `keccak256("OUTBE_METADATA_FINGERPRINT_V1" || canonical-encoded
    /// metadata economic fields)`. Same fingerprint observed twice for
    /// the same `fb_hash` is a replay no-op; different fingerprint for
    /// the same `fb_hash` is fatal (contradictory metadata for the same
    /// finalized block is a protocol violation).
    pub metadata_fingerprint_for_block: Mapping<B256, B256>,

    /// Per-finalized-block guard: fee dust from this block already
    /// accumulated into `daily_fee_dust`. Separate from
    /// `block_metadata_counted` so per-voter and per-block dust paths
    /// have independent short-circuits.
    pub fee_dust_counted_for_block: Mapping<B256, bool>,

    /// Idempotency guard for [`crate::api::add_topup_for_voters`]. Once
    /// the daily topup has been credited for a UTC day, subsequent calls
    /// for the same day are no-ops (return zero distributed dust). This
    /// is independent from `daily_settled`, which is owned by the future
    /// EmissionLimit `run_daily_dispatch` orchestrator and
    /// marks the entire daily dispatch as complete. Splitting the two
    /// keeps the api-level idempotency contract decoupled from the
    /// late-after-settle guard in `on_finalized_metadata`.
    pub daily_topup_settled: Mapping<u32, bool>,

    // ── per-block fee escrow + inclusion-window credits ──────────
    /// Escrowed fees of a finalized block (key `fb_hash`), recorded instead of
    /// the eager per-voter transfer. Settled at `N+K` across the credited voter
    /// set with a decay-weighted, fixed-denominator payout; residue burns.
    pub pending_fees: Mapping<B256, U256>,

    /// Per-finalized-block guard: the escrowed fee has been settled exactly once.
    pub fee_settled: Mapping<B256, bool>,

    /// Per-(fb_hash, voter) smallest inclusion distance, stored as `k + 1`
    /// (`0` = not yet credited) so the base 2f+1 seeded at `k=0` records `1` and
    /// a later re-inclusion at `k>=1` is a no-op.
    pub late_voter_k_plus1: Mapping<B256, Mapping<Address, u8>>,

    /// Per-fb_hash count of distinct credited voters — bound for index iteration
    /// over `late_voter_at` at settle time.
    pub late_voter_count: Mapping<B256, u32>,

    /// Per-(fb_hash, index) credited voter address, in first-credit order, so the
    /// settlement can enumerate the voter set deterministically.
    pub late_voter_at: Mapping<B256, Mapping<u32, Address>>,

    /// Settle-trigger lookup: finalized block number -> its `fb_hash`, so the
    /// `LateFinalizeCredits` phase at block `N+K` can settle block `N`'s escrow
    /// by number (the mandatory window-close side effect). `B256::ZERO` = absent.
    pub pending_fb_hash_at: Mapping<u64, B256>,

    /// Finalized block number -> committee size at its epoch — the fixed
    /// denominator basis (`D = committee_size * w_max`) for its settlement.
    pub pending_committee_size_at: Mapping<u64, u32>,

    /// Finalized block number -> its canonical consensus epoch
    /// The `LateFinalizeCredits` phase verifies a credit's
    /// proposer-supplied `epoch`/`committee_set_hash`/`fb_hash` against the
    /// canonical binding escrowed here, so `fb_number` cannot be spoofed to
    /// inflate the decay weight `k` or to reference a wrong committee.
    pub pending_epoch_at: Mapping<u64, u64>,

    /// Finalized block number -> its canonical `committee_set_hash`
    /// authentication; see `pending_epoch_at`).
    pub pending_committee_set_hash_at: Mapping<u64, B256>,

    /// Finalized block number -> its canonical consensus `view`
    /// authentication). Together with `pending_parent_view_at` this pins the FULL
    /// signed binding `{epoch, view, parent_view, fb_hash}` at the body, so a
    /// proposer cannot record a credit whose aggregate is over a non-canonical
    /// view of the same `fb_hash` (cross-view equivocation). The pre-exec BLS
    /// verify only ties the credit's view to its signatures; this ties it to the
    /// finalized certificate.
    pub pending_view_at: Mapping<u64, u64>,

    /// Finalized block number -> its canonical `parent_view`
    /// authentication; see `pending_view_at`).
    pub pending_parent_view_at: Mapping<u64, u64>,
}
