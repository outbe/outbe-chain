use super::open_logical_projection;
use super::test_support::admit_startup_finalized_target;
use super::test_support::run_durable_projection_writer;
use super::test_support::FinalizedTargetDisposition;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use super::apply_durable_projection_write_before;
use super::evaluate_ocomp_projection_containment;
use super::prepare_offchain_data_projection;
use super::projection_frame_failure_class;
use super::require_finalized_checkpoint;
use super::test_support::apply_durable_projection_write_until;
use super::test_support::project_through_target;
use super::test_support::projection_failure_class;
use super::test_support::record_finalized_target;
use super::test_support::record_or_publish_finalized_target;
use super::test_support::run_projection_loop;
use super::test_support::spawn_detached_projection_work;
use super::test_support::supervise_projection_future;
use super::validate_projection_network;
use super::DurableProjectionWrite;
use super::FinalizedProjectionSink;
use super::FinalizedTarget;
use super::FinalizedTargetReconciliationV1;
use super::OcompProjectionContainment;
use super::OffchainDataProjectionConfig;
use super::ProjectionRetentionFence;
use super::ProjectionRuntime;
use super::ProjectionRuntimeRecoveryHandle;
use super::ProjectionRuntimeRecoveryV1;
use super::ProjectionWriteDeadlineError;
use super::RuntimeBodyFailure;
use super::PROJECTION_RECOVERY_DEADLINE;
use crate::finalized_frame::{read_bounded_finalized_frames, RethFinalizedFrameSource};
use alloy_consensus::Header;
use alloy_eips::BlockNumHash;
use alloy_primitives::B256;
use outbe_offchain_data::{
    FinalizedBlock, OffchainDataProjection, ProjectionConfig, ProjectionFailure,
    ProjectionFailureClass,
};
use outbe_offchain_storage::{
    AtomicWriteBatch, AtomicWriteOperation, Key, MemoryStorage, Namespace, PendingOverlayStorage,
    ScanPage, ScanRequest, StorageError, StorageReader, StorageReaderHandle, StorageWriter,
    StorageWriterHandle, StoredValue,
};
use outbe_primitives::projection::{
    projection_readiness, ProjectionCheckpoint, ProjectionStatus, WaitOutcome,
};
use reth_ethereum::{exex::ExExEvent, Block};
use reth_provider::test_utils::MockEthProvider;

mod fixtures;
use fixtures::{
    add_empty_block, checkpoint, initialized_runtime, BlockingWriteStorage, FailAfterStartupStorage,
};

mod containment;

mod durability;

mod recovery;

mod startup;

mod targets;
