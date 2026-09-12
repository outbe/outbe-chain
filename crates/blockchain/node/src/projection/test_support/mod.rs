use super::FinalizedProjectionSink;
use super::ProjectionRuntime;

impl FinalizedProjectionSink {
    #[cfg(test)]
    pub(super) fn from_runtime(runtime: ProjectionRuntime) -> Self {
        Self::from_runtime_inner(runtime, None)
    }
}

mod runtime;
pub(super) use runtime::{
    project_through_target, run_projection_loop, spawn_detached_projection_work,
    supervise_projection_future, HistoricalProjectionDataError,
};

mod status;
pub(super) use status::{
    projection_failure_class, projection_is_unavailable, publish_fatal, publish_progress,
    publish_projection_failure,
};

mod targets;
pub(super) use targets::{
    admit_startup_finalized_target, record_finalized_target, record_or_publish_finalized_target,
    FinalizedTargetDisposition,
};

mod writer;
pub(super) use writer::{apply_durable_projection_write_until, run_durable_projection_writer};
