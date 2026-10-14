use super::classify_canonical_job;
use super::discovery_record;
use super::local_result_restore_policy;
use super::CanonicalJobDispositionV1;
use super::EmbeddedOcompExExV1;
use super::LocalResultRestorePolicyV1;
use super::LocalVoteEligibilityV1;
use super::RequestLocatorV1;
use super::RuntimeJobV1;

use alloy_primitives::B256;
use eyre::bail;
use eyre::Context as _;
use outbe_ocomp::discovery_control::DiscoveryOfferRefV1;
use outbe_ocomp::embedded::EmbeddedJobEventV1;
use outbe_ocomp::embedded::EmbeddedJobStateV1;
use outbe_ocomp::embedded::EmbeddedTerminalReasonV1;
use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;
use outbe_ocomp::supervisor::DiscoveryRecord;
use outbe_ocomp_protocol::profile::poc_schema_limits;
use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_primitives::OutbeReceipt;
use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;
use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;
use reth_provider::StateProviderFactory;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

type RetainedCandidate = (u64, outbe_node::ocomp::retention::FinalizedJobPinV1);

fn retained_pin_matches_finalized(
    retained: &outbe_node::ocomp::retention::FinalizedJobPinV1,
    locator: RequestLocatorV1,
    record: &OcompJobRecordV1,
    job_id: B256,
    input_lease_id: B256,
) -> bool {
    let Some(finalized) = record.finalized.as_ref() else {
        return false;
    };
    (
        retained.job_id,
        retained.candidate.block_number,
        retained.candidate.block_hash,
        retained.candidate.state_root,
        retained.candidate.intent_id,
        retained.candidate.wwd,
        retained.candidate.ce_sealed_root,
        retained.candidate.protocol_bundle_hash,
        retained.candidate.input_lease_id,
        retained.finality_recorded_height,
        retained.open_height,
        retained.deadline_height,
    ) == (
        job_id,
        locator.block_number,
        locator.block_hash,
        locator.state_root,
        locator.intent_id,
        locator.wwd,
        record.intent.ce_sealed_root,
        record.intent.protocol_bundle_hash,
        input_lease_id,
        finalized.finality_recorded_height,
        finalized.open_height,
        finalized.deadline_height,
    )
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
    pub(super) async fn refresh_jobs(
        &mut self,
        height: u64,
        hash: B256,
        drive_work: bool,
    ) -> eyre::Result<()> {
        let locators = self.requests.values().copied().collect::<Vec<_>>();
        for locator in locators {
            self.refresh_single_job(locator, height, hash, drive_work)
                .await?;
        }
        Ok(())
    }

    async fn refresh_single_job(
        &mut self,
        locator: RequestLocatorV1,
        height: u64,
        hash: B256,
        drive_work: bool,
    ) -> eyre::Result<()> {
        let limits = poc_schema_limits();
        let record = outbe_node::ocomp::retention::read_ocomp_job_record_at(
            &self.provider,
            hash,
            locator.intent_id,
            &limits,
        )
        .wrap_err("read current exact OCOMP job record")?;
        let disposition = classify_canonical_job(record.status, record.finalized.is_some())?;
        match disposition {
            CanonicalJobDispositionV1::AwaitingFinality => return Ok(()),
            CanonicalJobDispositionV1::Closed {
                has_finalized_job: false,
                ..
            } => {
                self.materialized_requests.insert(locator.intent_id);
                return Ok(());
            }
            CanonicalJobDispositionV1::FinalizedAwaitingOpen
            | CanonicalJobDispositionV1::VotingOpen
            | CanonicalJobDispositionV1::Completed
            | CanonicalJobDispositionV1::Closed {
                has_finalized_job: true,
                ..
            } => {}
        }
        let job_id = self.bind_finalized_job_identity(locator, &record, &limits)?;
        let discovered = !self.jobs.contains_key(&job_id);
        if discovered && !self.discover_job(locator, &record, job_id, disposition, &limits)? {
            return Ok(());
        }
        let export_acknowledged = self.reconcile_job_retention(
            (locator.intent_id, job_id),
            &record,
            disposition,
            (height, drive_work),
        )?;
        self.drive_job_disposition(
            locator,
            &record,
            job_id,
            (
                disposition,
                discovered,
                export_acknowledged,
                drive_work,
                height,
            ),
        )
        .await?;
        self.observe_deadline_if_reached(&record, job_id, height)
    }

    fn bind_finalized_job_identity(
        &mut self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> eyre::Result<B256> {
        let finalized = record
            .finalized
            .as_ref()
            .ok_or_else(|| eyre::eyre!("OCOMP finalized payload disappeared"))?;
        if (
            finalized.finalized_request_block_hash,
            finalized.finalized_request_state_root,
        ) != (locator.block_hash, locator.state_root)
        {
            bail!("OCOMP finalized job disagrees with its request block identity");
        }
        let job_id = record
            .intent
            .job_id(locator.block_hash, locator.state_root, limits)
            .wrap_err("derive finalized OCOMP JobId")?;
        if job_id != finalized.job_id {
            bail!("OCOMP finalized job carries a conflicting JobId");
        }
        if let Some(existing) = self.intent_jobs.insert(locator.intent_id, job_id) {
            if existing != job_id {
                bail!("OCOMP intent replay resolved to a conflicting JobId");
            }
        }
        Ok(job_id)
    }

    fn discover_job(
        &mut self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
        disposition: CanonicalJobDispositionV1,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> eyre::Result<bool> {
        let Some(retained_candidates) =
            self.load_retained_candidates(locator, record, job_id, disposition, limits)?
        else {
            return Ok(false);
        };
        let discovery =
            self.select_discovery_record(locator, record, job_id, retained_candidates, limits)?;
        let finalized = record
            .finalized
            .as_ref()
            .ok_or_else(|| eyre::eyre!("OCOMP finalized payload disappeared"))?;
        let generation = self
            .state
            .observe_job(job_id, finalized.deadline_height)
            .wrap_err("observe embedded OCOMP job")?;
        let vote_eligibility = if self.policy == EmbeddedNodePolicyV1::Validator {
            LocalVoteEligibilityV1::Pending
        } else {
            LocalVoteEligibilityV1::NotMember
        };
        self.jobs.insert(
            job_id,
            RuntimeJobV1 {
                record: discovery,
                generation,
                cancelled: Arc::new(AtomicBool::new(false)),
                compute_started: false,
                vote_eligibility,
                vote_started: false,
                canonical_result: None,
            },
        );
        Ok(true)
    }

    fn load_retained_candidates(
        &self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
        disposition: CanonicalJobDispositionV1,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> eyre::Result<Option<Vec<RetainedCandidate>>> {
        use outbe_node::ocomp::retention::RetentionError;
        match self.retention_selector.discovery_job_records(job_id) {
            Ok(records) => Ok(Some(records)),
            Err(RetentionError::RetentionCoordinatorNotInstalled) => Ok(None),
            Err(RetentionError::InvalidTransition(_)) => self
                .recover_retained_candidates(locator, record, job_id, disposition, limits)
                .map(Some),
            Err(error) => Err(error).wrap_err("load exact OCOMP retention generation"),
        }
    }

    fn recover_retained_candidates(
        &self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
        disposition: CanonicalJobDispositionV1,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> eyre::Result<Vec<RetainedCandidate>> {
        use outbe_node::ocomp::retention::RetentionError;
        let released = match self.retention_selector.released_job_authority(job_id) {
            Ok(released) => released,
            Err(RetentionError::InvalidTransition(_)) => None,
            Err(error) => return Err(error.into()),
        };
        let Some(released) = released else {
            self.retention_selector
                .bind_canonical_finalized_job(locator.block_hash, record)
                .wrap_err("bind OCOMP retention to canonical finalized typed state")?;
            return self
                .retention_selector
                .discovery_job_records(job_id)
                .wrap_err("load exact OCOMP retention generation after canonical binding");
        };
        if !matches!(
            disposition,
            CanonicalJobDispositionV1::Closed { .. } | CanonicalJobDispositionV1::Completed
        ) {
            bail!("released OCOMP job is not canonically terminal");
        }
        self.verify_released_export_ack(locator, record, job_id, limits)?;
        let finalized = record
            .finalized
            .as_ref()
            .ok_or_else(|| eyre::eyre!("OCOMP finalized payload disappeared"))?;
        Ok(vec![(
            released.source_generation,
            outbe_node::ocomp::retention::FinalizedJobPinV1 {
                candidate: released.candidate,
                job_id,
                finality_recorded_height: finalized.finality_recorded_height,
                open_height: finalized.open_height,
                deadline_height: finalized.deadline_height,
            },
        )])
    }

    fn select_discovery_record(
        &self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
        retained_candidates: Vec<RetainedCandidate>,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> eyre::Result<DiscoveryRecord> {
        let input_lease_id = record
            .intent
            .input_lease_id()
            .wrap_err("derive exact OCOMP input lease")?;
        let mut fallback = None;
        let mut selected = None;
        for (source_generation, retained) in retained_candidates {
            if !retained_pin_matches_finalized(&retained, locator, record, job_id, input_lease_id) {
                bail!("OCOMP retained pin disagrees with finalized typed state");
            }
            let candidate = discovery_record(locator, record, job_id, source_generation, limits)?;
            if self.candidate_has_durable_authority(&candidate, record, limits)? {
                if selected.replace(candidate).is_some() {
                    bail!("multiple OCOMP discovery generations have durable authority");
                }
            } else if fallback.is_none() {
                fallback = Some(candidate);
            }
        }
        selected
            .or(fallback)
            .ok_or_else(|| eyre::eyre!("OCOMP retention returned no discovery generation"))
    }

    fn candidate_has_durable_authority(
        &self,
        candidate: &DiscoveryRecord,
        record: &OcompJobRecordV1,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> eyre::Result<bool> {
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
            limits,
        )?;
        let pending = spool.pending(&probe.observation_id)?;
        let acknowledged = spool.ack(&probe.observation_id)?;
        let durable_reference = pending
            .as_ref()
            .map(|pending| pending.reference.clone())
            .or_else(|| acknowledged.as_ref().map(|ack| ack.reference.offer_ref()));
        Ok(durable_reference.as_ref() == Some(&probe))
    }

    fn reconcile_job_retention(
        &mut self,
        (intent_id, job_id): (B256, B256),
        record: &OcompJobRecordV1,
        disposition: CanonicalJobDispositionV1,
        (height, drive_work): (u64, bool),
    ) -> eyre::Result<bool> {
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
            self.reconcile_export_ack(job_id, record, drive_work)?
        };
        self.retention_selector
            .reconcile_canonical_terminal(record, height)?;
        if export_acknowledged {
            self.materialized_requests.insert(intent_id);
        }
        Ok(export_acknowledged)
    }

    async fn drive_job_disposition(
        &mut self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
        (disposition, discovered, export_acknowledged, drive_work, height): (
            CanonicalJobDispositionV1,
            bool,
            bool,
            bool,
            u64,
        ),
    ) -> eyre::Result<()> {
        match disposition {
            CanonicalJobDispositionV1::FinalizedAwaitingOpen => {}
            CanonicalJobDispositionV1::VotingOpen => {
                if export_acknowledged && drive_work {
                    self.drive_voting_open(locator, record, job_id, disposition, discovered)?;
                }
            }
            CanonicalJobDispositionV1::Completed => {
                if export_acknowledged {
                    self.drive_completed(record, job_id, disposition, discovered, height)
                        .await?;
                }
            }
            CanonicalJobDispositionV1::Closed { reason, .. } => {
                self.observe_terminal(job_id, reason)?;
            }
            CanonicalJobDispositionV1::AwaitingFinality => {
                unreachable!("awaiting-finality records returned before job discovery")
            }
        }
        Ok(())
    }

    fn drive_voting_open(
        &mut self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
        disposition: CanonicalJobDispositionV1,
        discovered: bool,
    ) -> eyre::Result<()> {
        let eligibility_became_available =
            self.refresh_vote_eligibility(locator, record, job_id)?;
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
        self.ensure_compute_started(job_id)
    }

    async fn drive_completed(
        &mut self,
        record: &OcompJobRecordV1,
        job_id: B256,
        disposition: CanonicalJobDispositionV1,
        discovered: bool,
        height: u64,
    ) -> eyre::Result<()> {
        self.observe_completed(job_id, record, height).await?;
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
        Ok(())
    }

    fn observe_deadline_if_reached(
        &mut self,
        record: &OcompJobRecordV1,
        job_id: B256,
        height: u64,
    ) -> eyre::Result<()> {
        let finalized = record
            .finalized
            .as_ref()
            .ok_or_else(|| eyre::eyre!("OCOMP finalized payload disappeared"))?;
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
        Ok(())
    }
}
