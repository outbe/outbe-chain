//! OCM-24 process-topology acceptance steps.
//!
//! The scenario extends the normal localnet lifecycle and launches only the
//! production `outbe-ocomp` executable. It cannot construct jobs, results,
//! roots or chain state.

use std::{
    path::{Path, PathBuf},
    str::FromStr as _,
    thread::{self, sleep},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use cucumber::{given, then, when};
use eyre::{ensure, eyre};
use outbe_chain_constants::GenesisProtocolParametersV1;
use outbe_node::ocomp::retention::{inspect_retention_journal, PinReleaseReason, PinStateV1};
use outbe_ocomp_protocol::{
    profile::poc_schema_limits,
    result::{ActiveNodSetV1, LysisResultV1, NodActionV1, NodMembershipProofV1, ResultChunkV1},
    state::{OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome},
    system_carrier::{MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS, OCOMP_SYSTEM_CARRIER_GAS_LIMIT},
    vote::ResultVoteV1,
};
use outbe_ocompregistry::{OcompProtocolAuthorityV1, OcompRequestProfile, OcompSuccessorV1};
use outbe_primitives::time::WorldwideDay;

use crate::features::common::{bootstrap_localnet, start_bootstrapped_localnet};
use crate::internal::addresses::UPDATE_ADDR;
use crate::internal::eth;
use crate::world::localnet::StartOpts;
use crate::world::ocomp::{
    OcompMeasurementForkV1, OcompNodeFacingResumePlan, OcompProcessFault, OcompProcessRole,
};
use crate::world::ocomp::{
    OCOMP_CAPACITY_OFFERING_AFTER_GENESIS_SECS, OCOMP_DYNAMIC_DKG_PREPARE_WINDOW_BLOCKS,
    OCOMP_DYNAMIC_VOTE_WINDOW_BLOCKS, OCOMP_PUBLIC_OFFERING_AFTER_GENESIS_SECS,
    OCOMP_PUBLIC_TRIBUTE_AMOUNT_BASE, OCOMP_PUBLIC_TRIBUTE_AMOUNT_MICRO,
    OCOMP_TEST_EPOCH_LENGTH_BLOCKS,
};
use crate::world::state::{
    MetadosisFinalizedPointV1, MetadosisFreshLifecycleObservationV1, MetadosisTimeControlEpochV1,
    OcompExecutionTraceObservationV1,
};
use crate::world::World;

mod capacity;
mod completion;
mod expiry;
mod full_node;
mod logical_time;
mod membership;
mod observations;
mod restart;
mod setup;
#[cfg(test)]
mod tests;
mod upgrade;

pub(crate) use completion::arm_case_one_artifact_phase;

pub(crate) use logical_time::restart_committee_at_logical_time;

pub(crate) use observations::completion::result_nod_actions_on;

use capacity::{OCOMP_CAPACITY_COMPLETION_TIMEOUT_SECS, OCOMP_CAPACITY_TRIBUTE_COUNT};

use completion::{
    assert_completed_replay_window_open, metadosis_creates_finalized_job_intent,
    quorum_applies_lysis_and_creates_nod, wait_case_one_worker_release,
    OCOMP_JOB_REQUEST_TIMEOUT_SECS,
};

use full_node::full_node_local_result_path;

use logical_time::{
    dynamic_oracle_refresh_timestamp, first_protocol_cycle_at_or_after, fresh_metadosis_wwd,
    logical_time_offset, post_restart_convergence_target,
    restart_ocomp_roles_after_committee_time_change, stop_ocomp_roles_before_committee_time_change,
    unix_time_secs, METADOSIS_FRESH_FORMING_SECONDS, RATCHET_STALL_TIMEOUT,
};

use observations::completion::{
    bounded_completion_decision, completed_accountability_is_preserved, dynamic_job_record,
    dynamic_pre_restart_vote_baseline_ready, dynamic_vote_submission_path, finalized_job_id,
    finalized_vote_for_delegate_on_job, local_result_path, observe_timely_completed_replay,
    quorum_applies_lysis_and_creates_nod_for_request, singleton_participant_bitmap,
    verify_case_one_completed_artifacts, wait_for_finalized_ocomp_activation,
    BoundedCompletionDecision, PublicVoteSetExpectation,
};

use observations::deadline::{
    accountability_slot_for_vote, assert_job_expires_without_nod, dynamic_deadline_account,
    dynamic_deadline_assert_live, dynamic_deadline_checkpoint, dynamic_deadline_decode_events,
    dynamic_deadline_ports, dynamic_deadline_validate_accountability,
    dynamic_deadline_validate_penalties, dynamic_deadline_validate_receipt,
    wait_for_released_retention, DynamicDeadlineAccount, DYNAMIC_OCOMP_RECOVERY_BLOCKS,
};

use observations::progress::{
    capture_ocomp_finality_before_fault, common_block_hash, finalized_points_at_common_height,
    finalized_points_at_height, monotonic_progress_decision, wait_for_common_finalized_checkpoint,
    ProgressWaitDecision, OCOMP_PROGRESS_STALL_TIMEOUT_SECS,
};

use upgrade::assert_pending_v1_at_common_finality;

#[cfg(test)]
use capacity::OCOMP_CAPACITY_SUBMISSION_CONCURRENCY;

#[cfg(test)]
use completion::case_one_compute_started_line;

#[cfg(test)]
use logical_time::{
    first_protocol_cycle_at_or_after_interval, fresh_wwd_lifecycle_overshot,
    historical_lifecycle_scan_heights, restart_barrier_decision, RestartBarrierDecision,
    RestartBarrierState,
};

#[cfg(test)]
use membership::joiner_restart_is_in_safe_early_epoch_window;

#[cfg(test)]
use observations::completion::{public_vote_set_matches, timely_replay_receipt_height};

#[cfg(test)]
use observations::deadline::{
    dynamic_deadline_storage_u64, dynamic_deadline_validate_checkpoints, retention_journal_root,
    DynamicDeadlineMiss,
};
