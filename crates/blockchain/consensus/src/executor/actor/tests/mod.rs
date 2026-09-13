use super::{wait_for_finalized_parent, wait_for_optional_ocomp_parent};
use super::{ExecutorActor, RecoveredForkchoiceAttempt};
use super::{FinalizedCeBlock, FinalizedCeCommitter, LastCanonicalized};
use crate::ancestry_readiness::AncestryReadiness;
use crate::block::ConsensusBlock;
use crate::digest::Digest;
use alloy_primitives::{Bytes, B256};
use alloy_rpc_types_engine::{PayloadStatus, PayloadStatusEnum};
use commonware_consensus::marshal::Update;
use commonware_consensus::types::Height;
use commonware_runtime::Clock;
use commonware_runtime::Runner as _;
use commonware_runtime::Spawner;
use commonware_runtime::Supervisor as _;
use commonware_utils::acknowledgement::Acknowledgement;
use commonware_utils::acknowledgement::Exact;
use outbe_primitives::{
    projection::{
        projection_readiness, ProjectionCheckpoint, ProjectionFailure, ProjectionFailureClass,
        ProjectionReadinessHandle, ProjectionReadinessPublisher, ProjectionStatus,
    },
    OutbeHeader,
};
use reth_ethereum::node::api::{BeaconEngineMessage, OnForkChoiceUpdated};
use reth_ethereum::{primitives::SealedBlock, Block};
use reth_node_builder::ConsensusEngineHandle;
use std::sync::{Arc, Mutex};

// -----------------------------------------------------------------------
// TC-2 (backfill fail-fast) helpers and regression test
// -----------------------------------------------------------------------

use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

mod fixtures;
use fixtures::{executor_test_block, ready_projection, ready_projection_for_block};

mod forkchoice;

mod readiness;

mod recovery;

mod finalization;

mod scheduling;

mod backfill;
