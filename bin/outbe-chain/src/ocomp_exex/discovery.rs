use super::classify_canonical_job;
use super::local_result_restore_policy;
use super::request_projection_is_closed;
use super::same_locator;
use super::CanonicalJobDispositionV1;
use super::EmbeddedOcompExExV1;
use super::LocalResultRestorePolicyV1;
use super::LocalVoteEligibilityV1;
use super::RequestLocatorV1;
use super::RuntimeJobV1;

use alloy_primitives::Address;
use alloy_primitives::B256;
use alloy_primitives::U256;

use eyre::bail;
use eyre::Context as _;

use metrics::counter;
use metrics::gauge;

use outbe_node::finalized_frame::FinalizedFrame;

use outbe_node::ocomp::retention::FinalizedRequestObservationV1;

use outbe_ocomp::discovery_control::DiscoveryOfferRefV1;

use outbe_ocomp::discovery_spool::RetirementReportV1;

use outbe_ocomp::embedded::EmbeddedJobEventV1;

use outbe_ocomp::embedded::EmbeddedJobStateV1;

use outbe_ocomp::embedded::EmbeddedTerminalReasonV1;

use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;

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

use std::sync::atomic::AtomicBool;

use std::sync::Arc;

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
        .ok_or_else(|| eyre::eyre!("OCOMP job is not finalized"))?;
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
                bail!("conflicting OCOMP request locator replay");
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
        if record.intent_height != locator.block_number
            || record.intent.wwd != locator.wwd
            || record.intent.pending_nonce != locator.pending_nonce
            || record.intent.attempt != locator.attempt
            || activation_hash != locator.activation_preconditions_hash
        {
            bail!("OCOMP request locator disagrees with exact typed state");
        }
        Ok(())
    }

    pub(super) async fn refresh_jobs(
        &mut self,
        height: u64,
        hash: B256,
        drive_work: bool,
    ) -> eyre::Result<()> {
        let limits = poc_schema_limits();
        let locators = self.requests.values().copied().collect::<Vec<_>>();
        for locator in locators {
            let record = outbe_node::ocomp::retention::read_ocomp_job_record_at(
                &self.provider,
                hash,
                locator.intent_id,
                &limits,
            )
            .wrap_err("read current exact OCOMP job record")?;
            let disposition = classify_canonical_job(record.status, record.finalized.is_some())?;
            match disposition {
                CanonicalJobDispositionV1::AwaitingFinality => continue,
                CanonicalJobDispositionV1::Closed {
                    has_finalized_job: false,
                    ..
                } => {
                    self.materialized_requests.insert(locator.intent_id);
                    continue;
                }
                CanonicalJobDispositionV1::FinalizedAwaitingOpen
                | CanonicalJobDispositionV1::VotingOpen
                | CanonicalJobDispositionV1::Completed
                | CanonicalJobDispositionV1::Closed {
                    has_finalized_job: true,
                    ..
                } => {}
            }
            let finalized = record
                .finalized
                .as_ref()
                .ok_or_else(|| eyre::eyre!("OCOMP finalized payload disappeared"))?;
            if finalized.finalized_request_block_hash != locator.block_hash
                || finalized.finalized_request_state_root != locator.state_root
            {
                bail!("OCOMP finalized job disagrees with its request block identity");
            }
            let job_id = record
                .intent
                .job_id(locator.block_hash, locator.state_root, &limits)
                .wrap_err("derive finalized OCOMP JobId")?;
            if job_id != finalized.job_id {
                bail!("OCOMP finalized job carries a conflicting JobId");
            }
            match self.intent_jobs.insert(locator.intent_id, job_id) {
                Some(existing) if existing != job_id => {
                    bail!("OCOMP intent replay resolved to a conflicting JobId");
                }
                Some(_) | None => {}
            }
            let discovered = !self.jobs.contains_key(&job_id);
            if discovered {
                let retained_candidates =
                    match self.retention_selector.discovery_job_records(job_id) {
                        Ok(record) => record,
                        Err(
                            outbe_node::ocomp::retention::RetentionError::RetentionCoordinatorNotInstalled,
                        ) => continue,
                        Err(outbe_node::ocomp::retention::RetentionError::InvalidTransition(_)) => {
                            let released = match self.retention_selector.released_job_authority(job_id) {
                                Ok(released) => released,
                                Err(outbe_node::ocomp::retention::RetentionError::InvalidTransition(_)) => None,
                                Err(error) => return Err(error.into()),
                            };
                            if let Some(released) = released {
                                if !matches!(disposition, CanonicalJobDispositionV1::Closed { .. } | CanonicalJobDispositionV1::Completed) {
                                    bail!("released OCOMP job is not canonically terminal");
                                }
                                self.verify_released_export_ack(locator, &record, job_id, &limits)?;
                                // Reconstruct the closed runtime projection as well as its
                                // intent mapping: retirement needs the exact original offer.
                                vec![(released.source_generation, outbe_node::ocomp::retention::FinalizedJobPinV1 {
                                    candidate: released.candidate,
                                    job_id,
                                    finality_recorded_height: finalized.finality_recorded_height,
                                    open_height: finalized.open_height,
                                    deadline_height: finalized.deadline_height,
                                })]
                            } else {
                                self.retention_selector
                                    .bind_canonical_finalized_job(locator.block_hash, &record)
                                    .wrap_err("bind OCOMP retention to canonical finalized typed state")?;
                                self.retention_selector
                                    .discovery_job_records(job_id)
                                    .wrap_err("load exact OCOMP retention generation after canonical binding")?
                            }
                        }
                        Err(error) => {
                            return Err(error).wrap_err("load exact OCOMP retention generation");
                        }
                    };
                let input_lease_id = record
                    .intent
                    .input_lease_id()
                    .wrap_err("derive exact OCOMP input lease")?;
                let mut fallback = None;
                let mut selected = None;
                for (source_generation, retained) in retained_candidates {
                    if retained.job_id != job_id
                        || retained.candidate.block_number != locator.block_number
                        || retained.candidate.block_hash != locator.block_hash
                        || retained.candidate.state_root != locator.state_root
                        || retained.candidate.intent_id != locator.intent_id
                        || retained.candidate.wwd != locator.wwd
                        || retained.candidate.ce_sealed_root != record.intent.ce_sealed_root
                        || retained.candidate.protocol_bundle_hash
                            != record.intent.protocol_bundle_hash
                        || retained.candidate.input_lease_id != input_lease_id
                        || retained.finality_recorded_height != finalized.finality_recorded_height
                        || retained.open_height != finalized.open_height
                        || retained.deadline_height != finalized.deadline_height
                    {
                        bail!("OCOMP retained pin disagrees with finalized typed state");
                    }
                    let candidate =
                        discovery_record(locator, &record, job_id, source_generation, &limits)?;
                    let spool = self
                        .discovery_spools
                        .get(&record.intent.protocol_bundle_hash)
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "OCOMP discovery spool is missing for bundle {}",
                                record.intent.protocol_bundle_hash
                            )
                        })?;
                    let probe = DiscoveryOfferRefV1::from_spec(
                        self.chain_id,
                        self.genesis_hash,
                        candidate.generation,
                        &candidate.spec,
                        &limits,
                    )?;
                    let pending = spool.pending(&probe.observation_id)?;
                    let acknowledged = spool.ack(&probe.observation_id)?;
                    let durable_reference = pending
                        .as_ref()
                        .map(|pending| pending.reference.clone())
                        .or_else(|| acknowledged.as_ref().map(|ack| ack.reference.offer_ref()));
                    let has_durable_authority = durable_reference.as_ref() == Some(&probe);
                    if has_durable_authority {
                        if selected.replace(candidate).is_some() {
                            bail!("multiple OCOMP discovery generations have durable authority");
                        }
                    } else if fallback.is_none() {
                        fallback = Some(candidate);
                    }
                }
                let discovery = selected.or(fallback).ok_or_else(|| {
                    eyre::eyre!("OCOMP retention returned no discovery generation")
                })?;
                let generation = self
                    .state
                    .observe_job(job_id, finalized.deadline_height)
                    .wrap_err("observe embedded OCOMP job")?;
                let cancelled = Arc::new(AtomicBool::new(false));
                self.jobs.insert(
                    job_id,
                    RuntimeJobV1 {
                        record: discovery,
                        generation,
                        cancelled,
                        compute_started: false,
                        vote_eligibility: if self.policy == EmbeddedNodePolicyV1::Validator {
                            LocalVoteEligibilityV1::Pending
                        } else {
                            LocalVoteEligibilityV1::NotMember
                        },
                        vote_started: false,
                        canonical_result: None,
                    },
                );
            }
            let canonical_expired = matches!(
                disposition,
                CanonicalJobDispositionV1::Closed {
                    reason: EmbeddedTerminalReasonV1::Expired,
                    ..
                }
            );
            let export_acknowledged = if canonical_expired {
                false
            } else {
                self.reconcile_export_ack(job_id, &record, drive_work)?
            };
            self.retention_selector
                .reconcile_canonical_terminal(&record, height)?;
            if export_acknowledged {
                self.materialized_requests.insert(locator.intent_id);
            }

            match disposition {
                CanonicalJobDispositionV1::FinalizedAwaitingOpen => {}
                CanonicalJobDispositionV1::VotingOpen => {
                    if !export_acknowledged || !drive_work {
                        continue;
                    }
                    let eligibility_became_available =
                        self.refresh_vote_eligibility(locator, &record, job_id)?;
                    let compute_started = self
                        .jobs
                        .get(&job_id)
                        .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
                        .compute_started;
                    if local_result_restore_policy(
                        disposition,
                        self.policy,
                        discovered,
                        eligibility_became_available,
                        compute_started,
                    ) == LocalResultRestorePolicyV1::BeforeCompute
                    {
                        self.restore_local_result(job_id)?;
                    }
                    self.ensure_compute_started(job_id)?;
                }
                CanonicalJobDispositionV1::Completed => {
                    if !export_acknowledged {
                        continue;
                    }
                    self.observe_completed(job_id, &record, height).await?;
                    if self.policy == EmbeddedNodePolicyV1::FullNode {
                        let compute_started = self
                            .jobs
                            .get(&job_id)
                            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
                            .compute_started;
                        if local_result_restore_policy(
                            disposition,
                            self.policy,
                            discovered,
                            false,
                            compute_started,
                        ) == LocalResultRestorePolicyV1::AfterCanonicalCompleted
                        {
                            self.restore_local_result(job_id)?;
                        }
                        self.ensure_compute_started(job_id)?;
                    }
                }
                CanonicalJobDispositionV1::Closed { reason, .. } => {
                    self.observe_terminal(job_id, reason)?;
                }
                CanonicalJobDispositionV1::AwaitingFinality => {
                    unreachable!("awaiting-finality records returned before job discovery")
                }
            }
            if height >= finalized.deadline_height
                && matches!(
                    self.state.state(job_id),
                    Some(
                        EmbeddedJobStateV1::Computing
                            | EmbeddedJobStateV1::LocalReady
                            | EmbeddedJobStateV1::WaitAtDeadline
                    )
                )
            {
                self.state
                    .reduce(job_id, EmbeddedJobEventV1::Deadline)
                    .wrap_err("observe OCOMP deadline")?;
            }
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
            .ok_or_else(|| eyre::eyre!("unified OCOMP scan height overflow"))?;
        if checkpoint.block_number != expected_number || frame.parent_hash() != self.scanned_hash {
            bail!("unified OCOMP frame scan is not contiguous");
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
            let job = self.jobs.get(job_id).ok_or_else(|| {
                eyre::eyre!("closed OCOMP job disappeared before spool retirement")
            })?;
            let bundle_hash = job.record.spec.summary.protocol_bundle_hash;
            let spool = self.discovery_spools.get(&bundle_hash).ok_or_else(|| {
                eyre::eyre!("closed OCOMP job references an unknown discovery spool")
            })?;
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
            bail!("OCOMP closure checkpoint is ahead of or conflicts with finalized target");
        }
        let ready_height = self
            .state
            .progress_limit(self.scanned_height.min(target.block_number));
        let ready_hash = self
            .provider
            .block_hash(ready_height)?
            .ok_or_else(|| eyre::eyre!("OCOMP readiness checkpoint is unavailable"))?;
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
