use super::request_projection_is_closed;
use super::same_locator;
use super::EmbeddedOcompExExV1;
use super::RequestLocatorV1;
use crate::errors::OcompExExError;

use alloy_primitives::Address;
use alloy_primitives::B256;
use alloy_primitives::U256;

use eyre::Context as _;

use metrics::counter;
use metrics::gauge;

use outbe_node::finalized_frame::FinalizedFrame;
use outbe_node::ocomp::retention::FinalizedRequestObservationV1;
use outbe_ocomp::discovery_control::DiscoveryOfferRefV1;
use outbe_ocomp::discovery_spool::RetirementReportV1;
use outbe_ocomp::supervisor::DiscoveryRecord;
use outbe_ocomp_protocol::common::BoundedBytes;
use outbe_ocomp_protocol::control::FinalizedJobSpecV1;
use outbe_ocomp_protocol::control::FinalizedJobSummaryV1;
use outbe_ocomp_protocol::profile::poc_schema_limits;
use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_primitives::projection::ProjectionCheckpoint;
use outbe_primitives::projection::ProjectionStatus;
use outbe_primitives::storage::readonly::StorageReader;
use outbe_primitives::OutbeReceipt;
use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;
use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;
use reth_provider::StateProvider;
use reth_provider::StateProviderFactory;

pub(super) fn finalized_reader_lags(finalized_head: u64, scanned: u64, closed: u64) -> (u64, u64) {
    (
        finalized_head.saturating_sub(scanned),
        finalized_head.saturating_sub(closed),
    )
}

pub(super) fn publish_finalized_reader_metrics(finalized_head: u64, scanned: u64, closed: u64) {
    let (reader_lag, closure_lag) = finalized_reader_lags(finalized_head, scanned, closed);
    gauge!("outbe_ocomp_finalized_head_number").set(finalized_head as f64);
    gauge!("outbe_ocomp_finalized_reader_checkpoint_number").set(scanned as f64);
    gauge!("outbe_ocomp_finalized_reader_lag_blocks").set(reader_lag as f64);
    gauge!("outbe_ocomp_closure_checkpoint_number").set(closed as f64);
    gauge!("outbe_ocomp_closure_lag_blocks").set(closure_lag as f64);
}

pub(super) fn record_discovery_retirement_report(report: RetirementReportV1) {
    if report.completed != 0 {
        counter!("outbe_ocomp_discovery_spool_retired_total").increment(report.completed);
    }
    gauge!("outbe_ocomp_discovery_spool_retirement_intents")
        .set(report.waiting_for_checkpoint as f64);
}

pub(super) fn discovery_record(
    locator: RequestLocatorV1,
    record: &OcompJobRecordV1,
    job_id: B256,
    generation: u64,
    limits: &outbe_ocomp_protocol::SchemaLimits,
) -> eyre::Result<DiscoveryRecord> {
    let finalized = record
        .finalized
        .as_ref()
        .ok_or(OcompExExError::JobNotFinalized)?;
    let spec = FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: locator.block_number,
            job_id,
            intent_id: locator.intent_id,
            finalized_block_hash: locator.block_hash,
            finalized_state_root: locator.state_root,
            protocol_bundle_hash: record.intent.protocol_bundle_hash,
            open_height: finalized.open_height,
            deadline_height: finalized.deadline_height,
        },
        canonical_job_intent: BoundedBytes(record.intent.encode_canonical(limits)?),
    };
    spec.encode_body(limits)?;
    Ok(DiscoveryRecord {
        generation,
        cursor: locator.block_number,
        spec,
    })
}

pub(super) struct OcompExExStateReaderV1<'a> {
    pub(super) state: &'a dyn StateProvider,
}

impl StorageReader for OcompExExStateReaderV1<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        self.state
            .storage(address, key)
            .map(|value| value.unwrap_or_default())
            .map_err(|error| {
                outbe_primitives::error::PrecompileError::Storage(format!(
                    "OCOMP ExEx finalized state read failed: {error}"
                ))
            })
    }
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
    pub(super) fn record_request_observation(
        &mut self,
        frame: &FinalizedFrame,
        observation: Option<FinalizedRequestObservationV1>,
    ) -> eyre::Result<()> {
        let Some(observation) = observation else {
            return Ok(());
        };
        let identity = frame.identity();
        let number = identity.number;
        let hash = identity.hash;
        let locator = RequestLocatorV1 {
            intent_id: observation.intent_id,
            wwd: observation.wwd,
            pending_nonce: observation.pending_nonce,
            attempt: observation.attempt,
            activation_preconditions_hash: observation.activation_preconditions_hash,
            block_number: number,
            block_hash: hash,
            state_root: frame.state_root(),
            before_request: ProjectionCheckpoint {
                block_number: number.saturating_sub(1),
                block_hash: frame.parent_hash(),
            },
        };
        self.validate_request(&locator, hash)?;
        match self.requests.get(&locator.intent_id) {
            Some(existing) if same_locator(*existing, locator) => {}
            Some(_) => {
                return Err(OcompExExError::ConflictingRequestLocatorReplay.into());
            }
            None => {
                self.requests.insert(locator.intent_id, locator);
            }
        }
        Ok(())
    }

    fn validate_request(&self, locator: &RequestLocatorV1, state_hash: B256) -> eyre::Result<()> {
        let limits = poc_schema_limits();
        let record = outbe_node::ocomp::retention::read_ocomp_job_record_at(
            &self.provider,
            state_hash,
            locator.intent_id,
            &limits,
        )
        .wrap_err("read exact OCOMP request record")?;
        let activation_hash = record
            .intent
            .activation_preconditions
            .activation_preconditions_hash(&limits)
            .wrap_err("hash OCOMP request activation preconditions")?;
        let expected = (
            locator.block_number,
            locator.wwd,
            locator.pending_nonce,
            locator.attempt,
            locator.activation_preconditions_hash,
        );
        let actual = (
            record.intent_height,
            record.intent.wwd,
            record.intent.pending_nonce,
            record.intent.attempt,
            activation_hash,
        );
        if actual != expected {
            return Err(OcompExExError::RequestLocatorDisagreesWithState.into());
        }
        Ok(())
    }

    pub(super) fn record_scanned_frame(&mut self, frame: &FinalizedFrame) -> eyre::Result<()> {
        let identity = frame.identity();
        let checkpoint = ProjectionCheckpoint {
            block_number: identity.number,
            block_hash: identity.hash,
        };
        let expected_number = self
            .scanned_height
            .checked_add(1)
            .ok_or(OcompExExError::UnifiedScanHeightOverflow)?;
        if checkpoint.block_number != expected_number || frame.parent_hash() != self.scanned_hash {
            return Err(OcompExExError::UnifiedFrameScanNotContiguous.into());
        }
        self.scanned_height = identity.number;
        self.scanned_hash = identity.hash;
        self.latest_scanned_checkpoint = checkpoint;
        Ok(())
    }

    pub(super) fn flush_closure_checkpoint(
        &mut self,
    ) -> eyre::Result<Option<ProjectionCheckpoint>> {
        let current = self.closure_checkpoint.current()?;
        let target = self
            .requests
            .values()
            .filter(|locator| !self.request_is_closed(locator.intent_id))
            .min_by_key(|locator| locator.block_number)
            .map_or(self.latest_scanned_checkpoint, |locator| {
                locator.before_request
            });
        if target.block_number <= current.block_number {
            self.prepare_closed_discovery_retirements(current.block_number)?;
            self.complete_discovery_retirements(current.block_number)?;
            self.prune_closed_requests_through(current.block_number)?;
            self.retention_selector
                .notify_closure_checkpoint(current.block_number);
            return Ok(None);
        }
        self.prepare_closed_discovery_retirements(target.block_number)?;
        self.complete_discovery_retirements(current.block_number)?;
        self.closure_checkpoint
            .compare_and_advance_to(current, target)?;
        self.complete_discovery_retirements(target.block_number)?;
        self.prune_closed_requests_through(target.block_number)?;
        self.retention_selector
            .notify_closure_checkpoint(target.block_number);
        Ok(Some(target))
    }

    fn prepare_closed_discovery_retirements(&self, closed_height: u64) -> eyre::Result<()> {
        for (intent_id, locator) in &self.requests {
            if locator.block_number > closed_height || !self.request_is_closed(*intent_id) {
                continue;
            }
            let Some(job_id) = self.intent_jobs.get(intent_id) else {
                continue;
            };
            let job = self
                .jobs
                .get(job_id)
                .ok_or(OcompExExError::ClosedJobDisappearedBeforeSpoolRetirement)?;
            let bundle_hash = job.record.spec.summary.protocol_bundle_hash;
            let spool = self
                .discovery_spools
                .get(&bundle_hash)
                .ok_or(OcompExExError::ClosedJobUnknownDiscoverySpool)?;
            let reference = DiscoveryOfferRefV1::from_spec(
                self.chain_id,
                self.genesis_hash,
                job.record.generation,
                &job.record.spec,
                &poc_schema_limits(),
            )?;
            spool.prepare_retirement(&reference, closed_height)?;
        }
        Ok(())
    }

    fn complete_discovery_retirements(&self, closed_height: u64) -> eyre::Result<()> {
        let mut aggregate = RetirementReportV1::default();
        for spool in self.discovery_spools.values() {
            let report = spool.complete_retirements_through(closed_height)?;
            aggregate.completed = aggregate.completed.saturating_add(report.completed);
            aggregate.waiting_for_checkpoint = aggregate
                .waiting_for_checkpoint
                .saturating_add(report.waiting_for_checkpoint);
        }
        record_discovery_retirement_report(aggregate);
        Ok(())
    }

    fn prune_closed_requests_through(&mut self, closed_height: u64) -> eyre::Result<()> {
        let closed_intents = self
            .requests
            .iter()
            .filter_map(|(intent_id, locator)| {
                (locator.block_number <= closed_height && self.request_is_closed(*intent_id))
                    .then_some(*intent_id)
            })
            .collect::<Vec<_>>();
        for intent_id in closed_intents {
            if let Some(job_id) = self.intent_jobs.get(&intent_id).copied() {
                self.state
                    .prune_terminal_job(job_id)
                    .wrap_err("prune exact terminal OCOMP projection")?;
            }
            self.requests.remove(&intent_id);
            self.materialized_requests.remove(&intent_id);
            if let Some(job_id) = self.intent_jobs.remove(&intent_id) {
                self.jobs.remove(&job_id);
                self.acknowledged_exports.remove(&job_id);
                self.pending_offers.remove(&job_id);
            }
        }
        Ok(())
    }

    fn request_is_closed(&self, intent_id: B256) -> bool {
        let job_id = self.intent_jobs.get(&intent_id).copied();
        request_projection_is_closed(
            self.materialized_requests.contains(&intent_id),
            job_id.and_then(|job_id| self.state.state(job_id)),
            job_id.and_then(|job_id| self.state.terminal_reason(job_id)),
        )
    }

    pub(super) fn publish_observation_progress(
        &self,
        target: ProjectionCheckpoint,
    ) -> eyre::Result<()> {
        let closed = self.closure_checkpoint.current()?;
        if closed.block_number > target.block_number
            || (closed.block_number == target.block_number
                && closed.block_hash != target.block_hash)
        {
            return Err(OcompExExError::ClosureCheckpointConflictsWithTarget.into());
        }
        let ready_height = self
            .state
            .progress_limit(self.scanned_height.min(target.block_number));
        let ready_hash = self
            .provider
            .block_hash(ready_height)?
            .ok_or(OcompExExError::ReadinessCheckpointUnavailable)?;
        let checkpoint = ProjectionCheckpoint {
            block_number: ready_height,
            block_hash: ready_hash,
        };
        self.readiness.publish(if checkpoint == target {
            ProjectionStatus::Ready { checkpoint }
        } else {
            ProjectionStatus::CatchingUp {
                checkpoint: Some(checkpoint),
            }
        });
        Ok(())
    }
}
