use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay as WorldwideDayKey;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::METADOSIS_ADDRESS;
use outbe_primitives::storage::types::{Mapping, StorageBytes, StorageVec};

/// EVM base slot of `MetadosisContract::ocomp_job_records`.
///
/// Earlier DSL collections occupy more than one slot, so this is deliberately
/// not the field's `order = 8`. OCM finality proof construction and verification
/// use this fixed consensus path; the storage behavior test pins it to the
/// macro-generated contract layout.
pub const OCOMP_JOB_RECORDS_BASE_SLOT: u64 = 21;

/// WorldwideDay status values stored as u8.
pub mod status {
    pub const FORMING: u8 = 0;
    pub const LOOKBACK_DELAY: u8 = 1;
    pub const OFFERING: u8 = 2;
    pub const WAITING: u8 = 3;
    pub const READY: u8 = 4;
    pub const IN_PROGRESS: u8 = 5;
    pub const COMPLETED: u8 = 6;
    pub const FAILED: u8 = 7;
    pub const OFFCHAIN_PENDING: u8 = 8;
}

/// Day type values.
pub mod day_type {
    pub const UNKNOWN: u8 = 0;
    pub const GREEN: u8 = 1;
    pub const RED: u8 = 2;
}

#[storage_record(exists_field = forming_start)]
pub struct WorldwideDay {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0, default = status::FORMING)]
    pub status: u8,

    #[attribute(order = 1, default = day_type::UNKNOWN)]
    pub day_type: u8,

    #[attribute(order = 2)]
    pub forming_start: u64,

    #[attribute(order = 3)]
    pub forming_end: u64,

    #[attribute(order = 4)]
    pub lookback_end: u64,

    #[attribute(order = 5)]
    pub offering_end: u64,

    #[attribute(order = 6)]
    pub scheduled_process_time: u64,

    #[attribute(order = 7, default = U256::ZERO)]
    pub metadosis_limit_amount: U256,

    #[attribute(order = 8, default = U256::ZERO)]
    pub previous_vwap: U256,

    #[attribute(order = 9, default = U256::ZERO)]
    pub current_vwap: U256,
}

/// Minimal PoC state proving which carry-over was consumed into an immutable
/// day limit. This is deliberately a separate append-only mapping so the
/// established `WorldwideDay` width and every later consensus slot remain
/// unchanged.
#[storage_record(exists_field = formed)]
pub struct OcompDayLimitFormationState {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0)]
    pub formed: bool,

    #[attribute(order = 1, default = U256::ZERO)]
    pub carry_over_taken: U256,
}

/// Fork-initialized OCOMP state owned by Metadosis for one WorldwideDay.
///
/// The detailed pre-admission values remain owned by Tribute, Fidelity and
/// Oracle. Metadosis commits their terminal canonical envelope and advances a
/// version used by later activation preconditions.
#[storage_record(exists_field = initialized)]
pub struct OcompPreAdmissionState {
    #[key]
    pub wwd: WorldwideDayKey,

    #[attribute(order = 0)]
    pub initialized: bool,

    #[attribute(order = 1)]
    pub state_version: u64,

    #[attribute(order = 2, default = B256::ZERO)]
    pub envelope_hash: B256,
}

/// EVM storage layout for the Metadosis orchestrator contract.
///
/// Manages worldwide day lifecycle and daily emission accumulation.
#[storage_schema]
#[contract(addr = METADOSIS_ADDRESS)]
pub struct MetadosisContract {
    #[attribute(order = 0)]
    pub bootstrap_end_time: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 1)]
    pub worldwide_days: outbe_primitives::storage::dsl::Map<WorldwideDayKey, WorldwideDay>,

    #[attribute(order = 2)]
    pub active_wwd_count: outbe_primitives::storage::dsl::Value<u16>,

    #[attribute(order = 3)]
    pub active_wwd: outbe_primitives::storage::dsl::Set<WorldwideDayKey>,

    /// Bounded FIFO of terminal (COMPLETED/FAILED) WorldwideDays, newest at the
    /// back. Capped at `MAX_RECORDS_KEPT`: when a new terminal day pushes past
    /// the cap, the oldest is popped from the front and its record deleted.
    #[attribute(order = 4)]
    pub closed_wwd: outbe_primitives::storage::dsl::Deque<WorldwideDayKey>,

    /// Inert before the OCOMP fresh-devnet fork initializes an entry.
    #[attribute(order = 5)]
    pub ocomp_pre_admission:
        outbe_primitives::storage::dsl::Map<WorldwideDayKey, OcompPreAdmissionState>,

    /// Fork-profile authority installed by the OCOMP upgrade handler. Empty
    /// before the disposable-devnet profile is armed.
    #[attribute(order = 6)]
    pub ocomp_request_profile: outbe_primitives::storage::types::StorageBytes,

    /// Exact bounded live-Job registry. Empty while no OCOMP intent is pending.
    /// READY work is kept in the separately bounded ordered index below; each
    /// live entry retains an independent per-WWD FSM and IntentId.
    #[attribute(order = 7)]
    pub ocomp_scheduler: outbe_primitives::storage::types::StorageBytes,

    /// Canonical OCB1 `OcompJobRecordV1`, keyed by
    /// `IntentStorageKeyV1 = H("OUTBE_OCOMP_INTENT_SLOT_V1", IntentId)`.
    #[attribute(order = 8)]
    pub ocomp_job_records: outbe_primitives::storage::types::Mapping<
        B256,
        outbe_primitives::storage::types::StorageBytes,
    >,

    /// Immutable request-phase budget receipt, keyed by WorldwideDay.
    #[attribute(order = 9)]
    pub ocomp_request_budget_receipts: outbe_primitives::storage::types::Mapping<
        WorldwideDayKey,
        outbe_primitives::storage::types::StorageBytes,
    >,

    /// Exact canonical pre-admission envelope committed by a created intent.
    #[attribute(order = 10)]
    pub ocomp_pre_admission_envelopes: outbe_primitives::storage::types::Mapping<
        WorldwideDayKey,
        outbe_primitives::storage::types::StorageBytes,
    >,

    /// Append-only terminal IntentIds. Reaching the profile cap rejects the
    /// transition; records are never silently evicted.
    #[attribute(order = 11)]
    pub ocomp_terminal_intents: outbe_primitives::storage::types::StorageVec<B256>,

    /// Canonical per-WWD FSM snapshots. A WWD has one exact READY or live
    /// state, while the global indexes below select bounded work without
    /// scanning `active_wwd`.
    #[attribute(order = 12)]
    pub ocomp_fsm_states: outbe_primitives::storage::types::Mapping<
        WorldwideDayKey,
        outbe_primitives::storage::types::StorageBytes,
    >,

    /// Canonical ordered READY keys `(next_check_height, WWD, pending_nonce)`.
    /// The encoded vector is bounded by `MAX_RECORDS_KEPT`; terminal request
    /// processing reads only its first key.
    #[attribute(order = 13)]
    pub ocomp_ready_index: outbe_primitives::storage::types::StorageBytes,

    /// PoC day-limit formation state. Appended after all existing fields so
    /// OCM finality proof paths and pre-fork storage offsets remain fixed.
    #[attribute(order = 14)]
    pub ocomp_day_limit_formations:
        outbe_primitives::storage::dsl::Map<WorldwideDayKey, OcompDayLimitFormationState>,

    /// Canonical active Lysis generation selected by completed Metadosis state.
    /// This append-only mapping is the public authority after activation.
    #[attribute(order = 15)]
    pub ocomp_active_lysis_generations: Mapping<WorldwideDayKey, StorageBytes>,

    /// Canonical OCB1 `ProtocolBundleV1` installed by the OCOMP fork handler.
    /// The request profile stores its hash; activation needs the complete
    /// immutable bundle to select the frozen LYSIS_V1 program semantics.
    #[attribute(order = 16)]
    pub ocomp_active_protocol_bundle: StorageBytes,

    /// Canonical OCB1 `OcompCommitteeSnapshotV1` installed with the bundle.
    /// Result votes are verified only against this consensus state.
    #[attribute(order = 17)]
    pub ocomp_result_committee_snapshot: StorageBytes,

    /// Four fixed result-vote slots and their independently closing
    /// accountability summary, keyed by finalized JobId.
    #[attribute(order = 18)]
    pub ocomp_vote_accountability: Mapping<B256, StorageBytes>,

    /// Canonical bounded response-window index ordered by
    /// `(deadline_height, JobId)`. It deliberately survives activation so the
    /// fourth validator and bounded equivocation evidence remain admissible
    /// until the exclusive deadline.
    #[attribute(order = 19)]
    pub ocomp_response_deadline_index: StorageBytes,

    /// Per-owner Fidelity league snapshot for one WorldwideDay, written once by
    /// the OCOMP prepare phase. Keyed by
    /// `outbe_ocomp_protocol::league_snapshot::league_snapshot_key(wwd, owner)`,
    /// it stores one league word per owner so the OCOMP openings MPT-prove a
    /// single league slot per owner instead of the raw Fidelity cohort ledger.
    /// Appended (base slot 34) so pre-fork storage offsets and OCM finality
    /// proof paths stay fixed; `league_snapshot::METADOSIS_LEAGUE_SNAPSHOT_BASE_SLOT`
    /// is pinned to this layout by test.
    #[attribute(order = 20)]
    pub ocomp_fidelity_league_snapshot: Mapping<B256, u16>,

    /// Ordered commitment over one day's snapshotted `(owner, league)` pairs,
    /// written alongside the per-owner snapshot during the active-phase prepare
    /// step. The post-seal terminal request reads it (a plain storage read valid
    /// in any lifecycle phase) to bind into the sealed pre-admission envelope. A
    /// non-zero value also marks the day's snapshot as already built.
    #[attribute(order = 21)]
    pub ocomp_fidelity_league_snapshot_root:
        outbe_primitives::storage::types::Mapping<WorldwideDayKey, B256>,
}
