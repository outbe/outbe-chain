use alloy_primitives::{Address, B256};
use outbe_macros::contract;
use outbe_primitives::addresses::SLASH_INDICATOR_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot};

/// EVM storage layout for the SlashIndicator precompile.
///
/// Storage slots:
///   0: config_proposer_misdemeanor_threshold - u64 (default 50)
///   1: config_proposer_felony_threshold      - u64 (default 150)
///   2: config_voter_misdemeanor_threshold    - u64 (default 150)
///   3: config_slash_amount_percent           - u64 (default 5)
///   4: config_evidence_reward_percent        - u64 (default 10)
///   5: proposer_miss_count                   - mapping(address => u64), per-epoch, resets
///   6: voter_miss_count                      - mapping(address => u64), per-epoch, resets
///   7: felony_count                          - mapping(address => u64), cumulative
/// 8: evidence_processed - mapping(B256 => bool), dedup
/// 9: voter_window_slashed - mapping(B256 => bool), per-finalized-block voter slash-window guard
/// 10: proposer_window_slashed - mapping(B256 => bool), per-finalized-block missed-proposer slash-window guard
///  11: invalid_vrf_evidence_processed        - mapping(B256 => bool) dedup keyed by `invalid_vrf_evidence_hash_v2(child_hash, phase1_tx_hash)`
///  12: config_voter_felony_threshold         - u64 (default 500).
///      Appended at the end to preserve the slot 0-11 layout.
///  13: seed_partial_equivocation_processed   - mapping(B256 => bool) dedup keyed by `SeedPartialEquivocationEvidence::dedup_hash`
///  14: invalid_seed_partial_processed        - mapping(B256 => bool) dedup keyed by `InvalidSeedPartialEvidence::dedup_hash`
/// 15: slash_guard_ring - mapping(uint64 => B256), prune ring of finalized fb_hashes
/// 16: slash_guard_ring_seq - uint64, ring write cursor
#[contract(addr = SLASH_INDICATOR_ADDRESS)]
pub struct SlashIndicator {
    // Config slots (0-4)
    pub config_proposer_misdemeanor_threshold: Slot<u64>,
    pub config_proposer_felony_threshold: Slot<u64>,
    pub config_voter_misdemeanor_threshold: Slot<u64>,
    pub config_slash_amount_percent: Slot<u64>,
    pub config_evidence_reward_percent: Slot<u64>,

    // Per-validator miss counters (slots 5-6), reset each epoch
    pub proposer_miss_count: Mapping<Address, u64>,
    pub voter_miss_count: Mapping<Address, u64>,

    // Cumulative felony count (slot 7), never reset
    pub felony_count: Mapping<Address, u64>,

    // Evidence dedup - tracks processed evidence hashes (slot 8)
    pub evidence_processed: Mapping<B256, bool>,

    // Per-finalized-block voter slash-window guard, keyed by
    // `metadata.finalized_block_hash`. The window-close absentee pass is atomic
    // per finalized block. The begin-zone system tx rolls back on revert. Thus a
    // single bool per `fb_hash` makes replays idempotent without an unbounded
    // per-voter nested mapping. The `slash_guard_ring` (slots 15/16) prunes this
    // guard.
    pub voter_window_slashed: Mapping<B256, bool>,

    // Per-finalized-block missed-proposer slash-window guard, keyed by
    // `fb_hash`. The Phase 1 missed-proposer pass processes the whole
    // `missed_proposers` list for one finalized parent atomically. Thus a single
    // bool per `fb_hash` is idempotent under metadata replay. The one pass still
    // slashes each duplicate proposer across skipped views. The
    // `slash_guard_ring` prunes this guard.
    pub proposer_window_slashed: Mapping<B256, bool>,

    // Dedup guard for `submitInvalidVrfProofEvidence`. Key is the
    // canonical evidence hash
    // `outbe_consensus::proof::invalid_vrf_evidence_hash_v2(child_hash, phase1_tx_hash)`.
    // A child block has exactly one Phase 1 system transaction. Thus this
    // pair encodes "one slash per (child, phase1)". A second submission of the
    // same evidence reverts with "evidence already processed". This matches
    // the precedent that `evidence_processed` sets for double-proposal and
    // conflicting-vote evidence (slot 8).
    pub invalid_vrf_evidence_processed: Mapping<B256, bool>,

    // Config (late addition, slot 12): voter felony threshold. The schema appends
    // it at the end so existing slots 0-11 keep their layout. `slash_voter`
    // jails and slashes a validator at multiples of this threshold. The
    // accessor returns the default (500) when the slot is unset (0). This
    // prevents `count % 0`.
    pub config_voter_felony_threshold: Slot<u64>,

    // Dedup guard for `submitSeedPartialEquivocationEvidence` (slot 13). Key is
    // `SeedPartialEquivocationEvidence::dedup_hash` (order-independent in the two
    // partials, bound to round + material version). Replaying the same
    // equivocation reverts with "evidence already processed", matching the
    // double-proposal / conflicting-vote / invalid-VRF precedents.
    pub seed_partial_equivocation_processed: Mapping<B256, bool>,

    // Dedup guard for `submitInvalidSeedPartialEvidence` (slot 14). Key is
    // `InvalidSeedPartialEvidence::dedup_hash` (round + version + signer +
    // partial), so each distinct invalid partial slashes at most once.
    pub invalid_seed_partial_processed: Mapping<B256, bool>,

    // Prune ring (slots 15/16). It bounds `voter_window_slashed` and
    // `proposer_window_slashed` to the last `SLASH_GUARD_RETAIN` finalized
    // blocks. The Phase 1 path drives the ring once per finalized block. That
    // path sees every `fb_hash` exactly once as a direct parent. The ring clears
    // both window guards of the entry evicted `SLASH_GUARD_RETAIN` records ago.
    // Retention is far larger than the K-block late-finalize window. Thus no
    // guard is dropped while its block can still be replayed. `B256::ZERO` =
    // empty slot.
    pub slash_guard_ring: Mapping<u64, B256>,
    pub slash_guard_ring_seq: Slot<u64>,
}
