use alloy_primitives::B256;

use eyre::Context as _;
use futures::StreamExt as _;

use outbe_node::projection::RuntimeBodyFailure;
use outbe_node::projection::PROJECTION_RECOVERY_DEADLINE;

use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;

use outbe_ocomp::embedded::EmbeddedJobStateV1;
use outbe_ocomp::embedded::EmbeddedOcompJobsV1;
use outbe_ocomp::embedded::EmbeddedOcompModeV1;
use outbe_ocomp::embedded::EmbeddedTerminalReasonV1;

use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;

use outbe_ocomp_protocol::state::OcompJobStatus;

use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionFailure;
use outbe_primitives::projection::ProjectionFailureClass;

use outbe_primitives::projection::ProjectionStatus;

use reth_ethereum::exex::ExExEvent;
use reth_ethereum::exex::ExExNotificationsStream;

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use std::sync::Arc;
use std::time::Duration;

use super::*;

mod canonical;

mod materialization;

mod recovery;
