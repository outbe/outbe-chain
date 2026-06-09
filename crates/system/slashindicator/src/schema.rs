use alloy_primitives::{Address, B256};
use outbe_macros::contract;
use outbe_primitives::addresses::SLASH_INDICATOR_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot};

/// EVM storage layout for the SlashIndicator precompile.
///
/// Storage slots:
///   0: config_proposer_misdemeanor_threshold — u64 (default 50)
///   1: config_proposer_felony_threshold      — u64 (default 150)
///   2: config_voter_misdemeanor_threshold    — u64 (default 500)
///   3: config_slash_amount_percent           — u64 (default 5)
///   4: config_evidence_reward_percent        — u64 (default 10)
///   5: proposer_miss_count                   — mapping(address => u64), per-epoch, resets
///   6: voter_miss_count                      — mapping(address => u64), per-epoch, resets
///   7: felony_count                          — mapping(address => u64), cumulative
///   8: evidence_processed                    — mapping(B256 => bool), A-03 dedup
///   9: slashed_voter_for_block               — mapping(B256 => mapping(address => bool)), per-finalized-block voter slash guard
///  10: slashed_proposer_event                — mapping(B256 => bool), per missed-proposer event guard
///  11: invalid_vrf_evidence_processed        — mapping(B256 => bool) dedup keyed by `invalid_vrf_evidence_hash_v2(child_hash, phase1_tx_hash)`
///  12: config_voter_felony_threshold         — u64 (default 150); appended at the end to preserve the slot 0-11 layout
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

    // A-03: Evidence dedup — tracks processed evidence hashes (slot 8)
    pub evidence_processed: Mapping<B256, bool>,

    // Per-finalized-block idempotency guard for voter slashing.
    // Keyed by `metadata.finalized_block_hash`; the inner mapping is keyed by
    // validator address. `slash_voter` short-circuits if the guard is already
    // set, so retried or replayed metadata for the same finalized block does
    // not double-count absent votes.
    pub slashed_voter_for_block: Mapping<B256, Mapping<Address, bool>>,

    // Phase 1 missed-proposer metadata is an event list: the same proposer can
    // appear multiple times for different skipped views before one finalized
    // block. This guard keys each event by `keccak256(fb_hash || index || addr)`
    // so exact metadata replays are idempotent without collapsing duplicates.
    pub slashed_proposer_event: Mapping<B256, bool>,

    // dedup guard for `submitInvalidVrfProofEvidence`. Key is the
    // canonical evidence hash
    // `outbe_consensus::proof::invalid_vrf_evidence_hash_v2(child_hash, phase1_tx_hash)`.
    // A child block has exactly one Phase 1 system transaction, so this
    // pair encodes "one slash per (child, phase1)" — submitting the same
    // evidence twice reverts with "evidence already processed", matching
    // the precedent set by `evidence_processed` for double-proposal and
    // conflicting-vote evidence (slot 8).
    pub invalid_vrf_evidence_processed: Mapping<B256, bool>,

    // Config (late addition, slot 12): voter felony threshold. Appended at the
    // end so existing slots 0-11 keep their layout. `slash_voter` force-exits
    // and slashes a validator at multiples of this threshold; the accessor
    // returns the default (150) when the slot is unset (0), avoiding `count % 0`.
    pub config_voter_felony_threshold: Slot<u64>,
}
