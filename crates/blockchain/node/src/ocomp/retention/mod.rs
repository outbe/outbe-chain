//! Crash-conservative wire-bounded multi-job OCOMP retention journal.
//!
//! Candidate discovery uses an event only as a bounded locator. The production
//! source re-opens the exact execution-valid block state and authenticates the
//! typed Metadosis record before this coordinator persists anything.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak,
    },
    time::{Duration, Instant},
};

use alloy_consensus::{BlockHeader as _, TxReceipt as _};
use alloy_primitives::{keccak256, B256, U256};
use alloy_sol_types::SolEvent as _;
use outbe_consensus::{
    block::ConsensusBlock,
    finalization::parent_cert_store::FinalizedParentCertStore,
    ocomp_retention::{OcompRetentionHook, OcompRetentionHookError},
};
use outbe_metadosis::{
    config::poc_schema_limits, precompile::IMetadosis, proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT,
};
use outbe_ocomp_protocol::{
    intent::{intent_storage_key, job_id_from_intent_id, FinalizedIntentProofV1, JobIntentV1},
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    state::{OcompJobRecordV1, OcompJobStatus},
    SchemaLimits,
};
use outbe_offchain_data::TributeRetentionSelector;
use outbe_offchain_storage::StorageErrorKind;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    error::PrecompileError,
    storage::{
        readonly::{ReadOnlyStorageProvider, StorageReader},
        types::StorageKey as _,
        StorageHandle,
    },
    OutbeHeader, OutbeReceipt,
};
pub use outbe_tribute::RetainedTributeWriter;
use outbe_tribute::{RetainedTributePin, TributeRepositoryError};
use reth_provider::{HeaderProvider, ReceiptProvider, StateProviderFactory};
use reth_storage_api::StateProvider;

use super::finality::RethFinalizedIntentProofBuilder;
use crate::{finalized_frame::FinalizedFrame, projection::ProjectionRetentionFence};

mod coordinator;
mod gc;
mod handles;
mod inspection;
mod journal;
mod source;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod types;

pub use coordinator::OcompRetentionCoordinator;

pub use handles::{OcompRetentionHandle, SharedOcompRetentionSelector};

pub use inspection::inspect_retention_journal;

pub use source::{
    observe_finalized_request, ocomp_snapshot_contains_key_at, read_ocomp_job_record_at,
    FinalizedInputProofSource, OcompSnapshotEligibilityV1, RethFinalizedInputProofSource,
};

pub use types::{
    CandidateFinalityV1, CandidatePinV1, DurablePinAck, ExportAuthorityV1, FinalizedJobPinV1,
    FinalizedRequestObservationV1, PinRecordV1, PinReleaseReason, PinStateV1,
    ReleasedJobAuthorityV1, RetentionError, RetentionJournalSnapshotV1, RetentionStatus,
};

#[cfg(test)]
pub use test_support::{
    FinalizedSnapshotArmer, OcompRetentionExecutionHandle, OcompRetentionService,
};

pub(crate) use gc::retained_gc_next_wake_delay;

pub(crate) use gc::retry::{journal_recovery_backoff, RetainedGcRetrySchedule, RetainedGcWorkId};

pub(crate) use journal::JournalDurability;

#[cfg(test)]
pub(crate) use test_support::{
    retention_pressure_watermark_for_test, seed_retention_journal_for_test,
    RetainedGcCycleTestReport,
};

pub(super) use types::retention_terminal_height_for_status;

use coordinator::{
    ack_for, candidate_job_id, record_candidate, JobRegistryV1, RETAINED_EVIDENCE_WINDOW_BLOCKS,
};

use gc::{record_journal_failure, spawn_retained_gc_worker, RetainedGcSignal};

use gc::retry::{
    classify_retained_gc_failure, RetainedGcAttemptFailure, RetainedGcAttemptOutcome,
    RetainedGcCycleFailure, RetainedGcCycleReport, RetainedGcFailureClass,
    RetainedGcScheduledCycle, RETAINED_GC_RETRY_BACKOFF,
};

use handles::hook_error;

use journal::{journal_successor_is_exact, JournalStore, OsJournalDurability, JOURNAL_FILENAME};

use journal::codec::{
    decode_registry, encode_record, encode_registry, JOURNAL_MAX_BYTES, JOURNAL_RECORD_COUNT_MAX,
};

use source::{canonical_finalized_pin, terminal_height_from_record};

use types::{retention_status_error, status_for_journal_error};
