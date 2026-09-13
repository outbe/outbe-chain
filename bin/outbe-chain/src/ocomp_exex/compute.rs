use super::advance_vote_eligibility;
use super::ignored_compute_result_reason;
use super::persist_local_failure_evidence;
use super::AsyncOutcomeProjectionV1;
use super::EmbeddedOcompExExV1;
use super::LocalVoteEligibilityV1;
use super::RequestLocatorV1;

use alloy_consensus::Transaction as _;
use alloy_consensus::TxReceipt as _;

use alloy_primitives::B256;

use alloy_sol_types::SolEvent as _;
use eyre::bail;
use eyre::Context as _;

use metrics::counter;

use outbe_metadosis::precompile::IMetadosis;
use outbe_node::finalized_frame::read_bounded_finalized_frames;
use outbe_node::finalized_frame::FinalizedFrame;
use outbe_node::finalized_frame::RethFinalizedFrameSource;

use outbe_ocomp::embedded::EmbeddedJobActionV1;
use outbe_ocomp::embedded::EmbeddedJobEventV1;
use outbe_ocomp::embedded::EmbeddedJobGenerationV1;

use outbe_ocomp::embedded::EmbeddedTerminalReasonV1;
use outbe_ocomp::embedded_runtime::EmbeddedComputeOutcomeV1;

use outbe_ocomp::embedded_runtime::EmbeddedNodePolicyV1;

use outbe_ocomp_protocol::profile::poc_schema_limits;

use outbe_ocomp_protocol::state::OcompJobRecordV1;

use outbe_ocomp_protocol::vote::decode_submit_lysis_result;
use outbe_ocomp_protocol::vote::ResultVoteV1;
use outbe_primitives::addresses::METADOSIS_ADDRESS;

use outbe_primitives::OutbeReceipt;

use reth_primitives_traits::Block as _;
use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;

use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;

use reth_provider::StateProviderFactory;

use std::sync::atomic::Ordering;

use std::sync::Arc;

use tracing::error;
use tracing::info;
use tracing::warn;

fn canonical_vote_from_frame(
    frame: &FinalizedFrame,
    quorum_height: u64,
    record: &OcompJobRecordV1,
    expected_digest: B256,
) -> eyre::Result<ResultVoteV1> {
    if frame.identity().number != quorum_height {
        bail!("q-forming OCOMP vote is not in the current finalized frame");
    }
    let limits = poc_schema_limits();
    let intent_id = record.intent.intent_id(&limits)?;
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| eyre::eyre!("completed OCOMP job is not finalized"))?;
    let mut matched = None;
    for (transaction, receipt) in frame.block().body().transactions().zip(frame.receipts()) {
        if !receipt.status() {
            continue;
        }
        let activates = receipt.logs().iter().any(|log| {
            if log.address != METADOSIS_ADDRESS
                || log.data.topics().first() != Some(&IMetadosis::LysisActivated::SIGNATURE_HASH)
            {
                return false;
            }
            IMetadosis::LysisActivated::decode_log(log).is_ok_and(|event| {
                event.data.intentId == intent_id
                    && event.data.jobId == finalized.job_id
                    && event.data.resultDigest == expected_digest
            })
        });
        if !activates || transaction.to() != Some(METADOSIS_ADDRESS) {
            continue;
        }
        let vote = decode_submit_lysis_result(transaction.input().as_ref(), &limits)
            .wrap_err("decode q-forming ResultVoteV1")?;
        let digest = vote.result_digest(&limits)?;
        vote.result.validate_finalized_intent(&record.intent)?;
        if vote.job_id != finalized.job_id
            || vote.protocol_bundle_hash != record.intent.protocol_bundle_hash
            || vote.attempt != record.intent.attempt
            || vote.result_validator_set_epoch != record.intent.result_validator_set_epoch
            || vote.result_committee_set_hash != record.intent.result_committee_set_hash
            || vote.result_ocomp_binding_hash != record.intent.result_ocomp_binding_hash
            || digest != expected_digest
        {
            bail!("q-forming ResultVoteV1 disagrees with finalized OCOMP authority");
        }
        if matched.replace(vote).is_some() {
            bail!("more than one q-forming OCOMP vote matched one completed job");
        }
    }
    matched.ok_or_else(|| eyre::eyre!("q-forming OCOMP ResultVoteV1 is missing"))
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
    pub(super) fn ensure_compute_started(&mut self, job_id: B256) -> eyre::Result<()> {
        let job = self
            .jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?;
        if job.compute_started {
            return Ok(());
        }
        self.domain.spawn_compute(
            job.record.clone(),
            job.generation,
            Arc::clone(&job.cancelled),
            self.compute_tx.clone(),
        )?;
        job.compute_started = true;
        info!(%job_id, "embedded OCOMP computation started");
        Ok(())
    }

    pub(super) fn restore_local_result(&mut self, job_id: B256) -> eyre::Result<()> {
        let Some(loaded) = self.domain.load_local_result(job_id)? else {
            return Ok(());
        };
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("restored OCOMP job is unknown"))?
            .compute_started = true;
        let generation = self
            .jobs
            .get(&job_id)
            .ok_or_else(|| eyre::eyre!("restored OCOMP job is unknown"))?
            .generation;
        self.accept_local_result(
            job_id,
            generation,
            loaded.committed.result_digest,
            loaded.canonical_result,
        )?;
        info!(
            %job_id,
            result_digest = %loaded.committed.result_digest,
            "restored durable embedded OCOMP local result"
        );
        Ok(())
    }

    pub(super) async fn observe_completed(
        &mut self,
        job_id: B256,
        record: &OcompJobRecordV1,
        finalized_height: u64,
    ) -> eyre::Result<()> {
        if self
            .jobs
            .get(&job_id)
            .and_then(|job| job.canonical_result.as_ref())
            .is_some()
        {
            return Ok(());
        }
        let terminal = record
            .terminal
            .as_ref()
            .ok_or_else(|| eyre::eyre!("completed OCOMP job lacks terminal record"))?;
        let binding = terminal
            .completed_binding
            .as_ref()
            .ok_or_else(|| eyre::eyre!("completed OCOMP job lacks completed binding"))?;
        if binding.quorum_height > finalized_height {
            bail!("OCOMP quorum is ahead of the reconciled finalized target");
        }
        let provider = self.provider.clone();
        let record = record.clone();
        let quorum_height = binding.quorum_height;
        let result_digest = binding.result_digest;
        let vote = tokio::task::spawn_blocking(move || {
            let quorum_hash = provider
                .block_hash(quorum_height)?
                .ok_or_else(|| eyre::eyre!("OCOMP quorum block is unavailable"))?;
            let source = RethFinalizedFrameSource::new(provider);
            let batch = read_bounded_finalized_frames(
                &source,
                quorum_height,
                (quorum_height, quorum_hash).into(),
            )?
            .ok_or_else(|| eyre::eyre!("OCOMP quorum frame is unavailable"))?;
            let frame = batch
                .frames()
                .first()
                .ok_or_else(|| eyre::eyre!("OCOMP quorum frame is missing"))?;
            canonical_vote_from_frame(frame, quorum_height, &record, result_digest)
        })
        .await
        .wrap_err("OCOMP quorum reader worker failed")??;
        let canonical = vote.result;
        let digest = canonical.result_digest(&poc_schema_limits())?;
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
            .canonical_result = Some(canonical.clone());
        let action = self
            .state
            .reduce(
                job_id,
                EmbeddedJobEventV1::CanonicalCompleted {
                    result_digest: digest,
                },
            )
            .wrap_err("record canonical OCOMP result")?
            .action;
        if matches!(action, EmbeddedJobActionV1::ReleaseProgress { .. }) {
            self.verify_full_node_exact(job_id, &canonical)?;
        } else if let EmbeddedJobActionV1::FatalMismatch {
            local_result_digest,
            canonical_result_digest,
            ..
        } = action
        {
            self.persist_and_publish_mismatch(
                job_id,
                local_result_digest,
                canonical_result_digest,
            )?;
        }
        Ok(())
    }

    pub(super) fn observe_terminal(
        &mut self,
        job_id: B256,
        reason: EmbeddedTerminalReasonV1,
    ) -> eyre::Result<()> {
        let first_observation = self.state.terminal_reason(job_id).is_none();
        self.state
            .reduce(job_id, EmbeddedJobEventV1::CanonicalClosed { reason })
            .wrap_err("record finalized OCOMP terminal state")?;
        if let Some(job) = self.jobs.get(&job_id) {
            job.cancelled.store(true, Ordering::Release);
        }
        if first_observation && reason == EmbeddedTerminalReasonV1::Expired {
            counter!("outbe_ocomp_canonical_jobs_cancelled_total", "reason" => "expired")
                .increment(1);
        }
        info!(%job_id, ?reason, "embedded OCOMP job reached canonical terminal state");
        Ok(())
    }

    pub(super) fn handle_compute(&mut self, outcome: EmbeddedComputeOutcomeV1) -> eyre::Result<()> {
        let (generation, completed) = match outcome {
            EmbeddedComputeOutcomeV1::Completed {
                generation,
                completed,
            } => (generation, completed),
            EmbeddedComputeOutcomeV1::Unrecoverable {
                job_id,
                generation,
                detail,
            } => {
                let Some(projection) = self.async_outcome_projection(job_id)? else {
                    return Ok(());
                };
                if projection == AsyncOutcomeProjectionV1::CheckpointPruned {
                    info!(%job_id, %detail, "ignored checkpoint-pruned OCOMP computation failure");
                    return Ok(());
                }
                let action = self
                    .state
                    .reduce(job_id, EmbeddedJobEventV1::LocalFailed { generation })
                    .wrap_err("reduce embedded OCOMP local failure")?
                    .action;
                match action {
                    EmbeddedJobActionV1::FatalLocalFailure => {
                        persist_local_failure_evidence(
                            self.domain.fatal_evidence_root(),
                            job_id,
                            &detail,
                        )?;
                        self.latch_fatal(job_id, detail)?;
                    }
                    EmbeddedJobActionV1::Abstain => {
                        error!(%job_id, %detail, "Validator OCOMP computation failed; abstaining from vote");
                    }
                    EmbeddedJobActionV1::ProtocolOwned => {
                        info!(%job_id, %detail, "ignored late embedded OCOMP computation failure");
                    }
                    _ => {
                        bail!("unexpected embedded OCOMP local-failure action");
                    }
                }
                return Ok(());
            }
        };
        let job_id = completed.job_id;
        let Some(projection) = self.async_outcome_projection(job_id)? else {
            return Ok(());
        };
        if let Some(reason) =
            ignored_compute_result_reason(projection, self.state.terminal_reason(job_id))
        {
            counter!("outbe_ocomp_late_compute_results_ignored_total", "reason" => reason)
                .increment(1);
            info!(%job_id, reason, "ignored late OCOMP result before local persistence");
            return Ok(());
        }
        let committed = self.domain.commit_local_result(&completed)?;
        self.accept_local_result(
            job_id,
            generation,
            committed.result_digest,
            completed.canonical_result,
        )
    }

    fn accept_local_result(
        &mut self,
        job_id: B256,
        generation: EmbeddedJobGenerationV1,
        result_digest: B256,
        canonical_result: Vec<u8>,
    ) -> eyre::Result<()> {
        let action = self
            .state
            .reduce(
                job_id,
                EmbeddedJobEventV1::LocalCompleted {
                    generation,
                    result_digest,
                },
            )
            .wrap_err("record embedded OCOMP local result")?
            .action;
        match action {
            EmbeddedJobActionV1::SubmitVote { .. } => {
                let job = self
                    .jobs
                    .get_mut(&job_id)
                    .ok_or_else(|| eyre::eyre!("computed OCOMP job is unknown"))?;
                match job.vote_eligibility {
                    LocalVoteEligibilityV1::Pending => {
                        info!(%job_id, "pinned OCOMP vote membership is temporarily unavailable; retaining local result");
                    }
                    LocalVoteEligibilityV1::NotMember => {
                        if !job.vote_started {
                            info!(%job_id, "embedded OCOMP Validator is not in the pinned snapshot; abstaining");
                            job.vote_started = true;
                        }
                    }
                    LocalVoteEligibilityV1::Eligible if !job.vote_started => {
                        self.domain.spawn_validator_vote(
                            job.record.clone(),
                            job.generation,
                            result_digest,
                            canonical_result,
                            Arc::clone(&job.cancelled),
                            self.vote_tx.clone(),
                        )?;
                        job.vote_started = true;
                    }
                    LocalVoteEligibilityV1::Eligible => {}
                }
            }
            EmbeddedJobActionV1::ReleaseProgress { .. } => {
                let canonical = self
                    .jobs
                    .get(&job_id)
                    .and_then(|job| job.canonical_result.clone())
                    .ok_or_else(|| eyre::eyre!("canonical OCOMP result disappeared"))?;
                self.verify_full_node_exact(job_id, &canonical)?;
            }
            EmbeddedJobActionV1::FatalMismatch {
                local_result_digest,
                canonical_result_digest,
                ..
            } => self.persist_and_publish_mismatch(
                job_id,
                local_result_digest,
                canonical_result_digest,
            )?,
            EmbeddedJobActionV1::AwaitCanonical { .. } => {}
            // A quorum can form before the local computation finishes; the job
            // is then already canonical-settled and no local action remains.
            EmbeddedJobActionV1::ProtocolOwned => {
                info!(
                    %job_id,
                    %result_digest,
                    "embedded OCOMP local result arrived after canonical settlement; protocol owns the job"
                );
            }
            EmbeddedJobActionV1::AwaitLocalResult { .. }
            | EmbeddedJobActionV1::HoldProgress { .. }
            | EmbeddedJobActionV1::CloseTerminal { .. }
            | EmbeddedJobActionV1::Abstain
            | EmbeddedJobActionV1::FatalLocalFailure
            | EmbeddedJobActionV1::VoteFinalized { .. }
            | EmbeddedJobActionV1::FatalVoteFailure => {
                bail!("unexpected embedded OCOMP local-result action");
            }
        }
        Ok(())
    }

    pub(super) fn refresh_vote_eligibility(
        &mut self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
    ) -> eyre::Result<bool> {
        if self.policy != EmbeddedNodePolicyV1::Validator {
            return Ok(false);
        }
        let current = self
            .jobs
            .get(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
            .vote_eligibility;
        if current != LocalVoteEligibilityV1::Pending {
            return Ok(false);
        }
        let Some(ocomp_key_hash) = self.domain.validator_ocomp_key_hash() else {
            bail!("Validator OCOMP key hash is unavailable");
        };
        let observed = outbe_node::ocomp::retention::ocomp_snapshot_contains_key_at(
            &self.provider,
            locator.block_hash,
            &record.intent,
            ocomp_key_hash,
        );
        if let outbe_node::ocomp::retention::OcompSnapshotEligibilityV1::Unavailable { detail } =
            &observed
        {
            warn!(%job_id, %detail, "pinned OCOMP vote membership is temporarily unavailable");
        }
        let resolved = advance_vote_eligibility(current, observed)?;
        if resolved == LocalVoteEligibilityV1::Pending {
            return Ok(false);
        }
        self.jobs
            .get_mut(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared"))?
            .vote_eligibility = resolved;
        Ok(resolved == LocalVoteEligibilityV1::Eligible)
    }
}
