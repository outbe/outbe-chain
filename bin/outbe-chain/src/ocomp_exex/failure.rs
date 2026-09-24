use super::EmbeddedOcompExExV1;
use super::OcompExExExitV1;

use alloy_primitives::B256;

use eyre::bail;
use eyre::Context as _;

use futures::FutureExt as _;
use futures::StreamExt as _;

use metrics::gauge;

use outbe_node::projection::projection_frame_failure_class;

use outbe_node::projection::ProjectionRuntimeRecoveryHandle;
use outbe_node::projection::ProjectionRuntimeRecoveryV1;

use outbe_node::projection::RuntimeBodyFailure;
use outbe_node::projection::PROJECTION_RECOVERY_DEADLINE;

use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionFailure;
use outbe_primitives::projection::ProjectionFailureClass;

use outbe_primitives::projection::ProjectionStatus;

use outbe_primitives::OutbeReceipt;

use reth_ethereum::exex::ExExEvent;
use reth_ethereum::exex::ExExNotificationsStream;

use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;

use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;

use reth_provider::StateProviderFactory;

use std::fs::DirBuilder;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read as _;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use tracing::error;

pub(super) async fn projection_runtime_failure(
    receiver: &tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>,
    unavailable_since: &mut Option<(u64, tokio::time::Instant)>,
    recovery: &ProjectionRuntimeRecoveryHandle,
    recovery_task: &mut Option<tokio::task::JoinHandle<ProjectionRuntimeRecoveryV1>>,
) -> Option<ProjectionFailure> {
    if recovery_task
        .as_ref()
        .is_some_and(tokio::task::JoinHandle::is_finished)
    {
        match recovery_task.take().expect("finished task exists").await {
            Ok(
                ProjectionRuntimeRecoveryV1::Recovered | ProjectionRuntimeRecoveryV1::Unavailable,
            ) => {}
            Ok(ProjectionRuntimeRecoveryV1::Fatal(failure)) => return Some(failure),
            Err(error) => {
                return Some(ProjectionFailure::new(
                    ProjectionFailureClass::Other,
                    format!("offchain runtime-body recovery worker failed: {error}"),
                ));
            }
        }
    }
    match receiver.borrow().clone() {
        None => {
            *unavailable_since = None;
            None
        }
        Some(RuntimeBodyFailure::Fatal(failure)) => Some(failure),
        Some(RuntimeBodyFailure::Unavailable { generation, since }) => {
            let since = tokio::time::Instant::from_std(since);
            *unavailable_since = Some((generation, since));
            if tokio::time::Instant::now() >= since + PROJECTION_RECOVERY_DEADLINE {
                return Some(projection_runtime_deadline_failure());
            }
            if recovery_task.is_none() {
                let recovery = recovery.clone();
                *recovery_task = Some(tokio::task::spawn_blocking(move || {
                    recovery.reconcile(generation)
                }));
            }
            None
        }
    }
}

fn projection_runtime_deadline_failure() -> ProjectionFailure {
    ProjectionFailure::new(
        ProjectionFailureClass::MongoReconnectDeadline,
        "offchain runtime-body storage remained unavailable past the recovery deadline",
    )
}

pub(super) fn consume_projection_runtime_deadline(
    receiver: &tokio::sync::watch::Receiver<Option<RuntimeBodyFailure>>,
    state: &mut Option<(u64, tokio::time::Instant)>,
) -> Option<ProjectionFailure> {
    let (expected_generation, since) = state.take()?;
    if tokio::time::Instant::now() < since + PROJECTION_RECOVERY_DEADLINE {
        return None;
    }
    matches!(
        receiver.borrow().clone(),
        Some(RuntimeBodyFailure::Unavailable { generation, .. })
            if generation == expected_generation
    )
    .then(projection_runtime_deadline_failure)
}

pub(super) fn projection_runtime_watch_closed_failure() -> ProjectionFailure {
    ProjectionFailure::new(
        ProjectionFailureClass::ReadinessChannelClosed,
        "offchain runtime-body failure channel closed",
    )
}

pub(super) fn projection_task_failure(error: eyre::Report) -> ProjectionFailure {
    let class = projection_frame_failure_class(&error);
    ProjectionFailure::new(
        class,
        format!("unified durable projection failed: {error:#}"),
    )
}

pub(super) fn without_execution_backfill<N, S>(mut notifications: S) -> S
where
    N: reth_primitives_traits::NodePrimitives,
    S: ExExNotificationsStream<N>,
{
    notifications.set_without_head();
    notifications
}

pub(super) async fn drain_exex_notifications<S, T>(mut notifications: S) -> eyre::Report
where
    S: futures::Stream<Item = eyre::Result<T>> + Unpin,
{
    while let Some(notification) = notifications.next().await {
        if let Err(error) = notification {
            return error.wrap_err("OCOMP live notification stream failed");
        }
    }
    eyre::eyre!("OCOMP live notification stream closed")
}

pub(super) fn publish_finished_height(
    events: &tokio::sync::mpsc::UnboundedSender<ExExEvent>,
    checkpoint: ProjectionCheckpoint,
) -> eyre::Result<()> {
    events
        .send(ExExEvent::FinishedHeight(
            (checkpoint.block_number, checkpoint.block_hash).into(),
        ))
        .wrap_err("OCOMP FinishedHeight consumer is unavailable")
}

pub(super) fn finished_height_closed_during_shutdown(
    error: &eyre::Report,
    shutdown: &reth_ethereum::tasks::shutdown::Shutdown,
) -> bool {
    error
        .downcast_ref::<tokio::sync::mpsc::error::SendError<ExExEvent>>()
        .is_some()
        && shutdown.clone().now_or_never().is_some()
}

pub(super) fn persist_fatal_evidence(
    root: &Path,
    job_id: B256,
    local: B256,
    canonical: B256,
) -> eyre::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700).recursive(true).create(root)?;
    let path = root.join(format!("{}.mismatch-v1", hex::encode(job_id.as_slice())));
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    writeln!(file, "job_id={job_id}")?;
    writeln!(file, "local_result_digest={local}")?;
    writeln!(file, "canonical_result_digest={canonical}")?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

const MAX_FATAL_EVIDENCE_BYTES: u64 = 64 * 1024;

pub(super) fn persist_generic_fatal_evidence(
    root: &Path,
    job_id: B256,
    detail: &str,
) -> eyre::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700).recursive(true).create(root)?;
    let path = root.join("sticky-fatal-v1");
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    writeln!(file, "job_id={job_id}")?;
    writeln!(file, "detail={detail}")?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

pub(super) fn load_persisted_fatal_evidence(root: &Path) -> eyre::Result<Option<String>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let Some(path) = paths.into_iter().next() else {
        return Ok(None);
    };
    let file = File::open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_FATAL_EVIDENCE_BYTES {
        bail!("embedded OCOMP fatal evidence has an invalid shape");
    }
    let mut detail = String::new();
    file.take(MAX_FATAL_EVIDENCE_BYTES + 1)
        .read_to_string(&mut detail)?;
    if detail.is_empty() || detail.len() as u64 > MAX_FATAL_EVIDENCE_BYTES {
        bail!("embedded OCOMP fatal evidence has an invalid length");
    }
    Ok(Some(detail))
}

pub(super) fn persist_local_failure_evidence(
    root: &Path,
    job_id: B256,
    detail: &str,
) -> eyre::Result<()> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700).recursive(true).create(root)?;
    let path = root.join(format!("{}.fatal-local-v1", hex::encode(job_id.as_slice())));
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    writeln!(file, "job_id={job_id}")?;
    writeln!(file, "detail={detail}")?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

impl<P> EmbeddedOcompExExV1<P>
where
    P: BlockIdReader
        + BlockHashReader
        + BlockReader
        + ReceiptProvider<Receipt = OutbeReceipt>
        + StateProviderFactory
        + Clone
        + Send
        + Sync
        + 'static,
{
    pub(super) fn persist_and_publish_mismatch(
        &mut self,
        job_id: B256,
        local: B256,
        canonical: B256,
    ) -> eyre::Result<()> {
        persist_fatal_evidence(self.domain.fatal_evidence_root(), job_id, local, canonical)?;
        self.latch_fatal(
            job_id,
            format!("local result {local} differs from canonical result {canonical}"),
        )
    }

    pub(super) fn latch_fatal(&mut self, job_id: B256, detail: String) -> eyre::Result<()> {
        if self.fatal.is_some() {
            return Ok(());
        }
        persist_generic_fatal_evidence(self.domain.fatal_evidence_root(), job_id, &detail)?;
        error!(%job_id, %detail, "embedded OCOMP requested local node shutdown");
        let failure = ProjectionFailure::new(
            ProjectionFailureClass::Other,
            format!("embedded OCOMP job {job_id}: {detail}"),
        );
        let allowed_height = self.state.progress_limit(self.scanned_height);
        let checkpoint = if allowed_height == self.scanned_height {
            Some(ProjectionCheckpoint {
                block_number: allowed_height,
                block_hash: self.scanned_hash,
            })
        } else {
            self.provider
                .block_hash(allowed_height)
                .ok()
                .flatten()
                .map(|block_hash| ProjectionCheckpoint {
                    block_number: allowed_height,
                    block_hash,
                })
        };
        self.fatal = Some(failure.clone());
        gauge!("outbe_ocomp_finalized_loop_fatal").set(1.0);
        self.readiness.publish(ProjectionStatus::Fatal {
            checkpoint,
            error: failure.clone(),
        });
        let _ = self.exit.send(OcompExExExitV1 { failure });
        Ok(())
    }

    pub(super) fn latch_external_failure(&mut self, failure: ProjectionFailure) {
        if self.fatal.is_some() {
            return;
        }
        self.fatal = Some(failure.clone());
        gauge!("outbe_ocomp_finalized_loop_fatal").set(1.0);
        self.readiness.publish(ProjectionStatus::Fatal {
            checkpoint: Some(ProjectionCheckpoint {
                block_number: self.scanned_height,
                block_hash: self.scanned_hash,
            }),
            error: failure.clone(),
        });
        let _ = self.exit.send(OcompExExExitV1 { failure });
    }
}
