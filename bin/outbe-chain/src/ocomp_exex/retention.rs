use super::discovery_record;
use super::released_export_authority_for_status;
use super::EmbeddedOcompExExV1;
use super::RequestLocatorV1;

use alloy_primitives::B256;

use eyre::bail;
use eyre::Context as _;

use outbe_ocomp::discovery_control::DiscoveryOfferRefV1;

use outbe_ocomp_protocol::profile::poc_schema_limits;

use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_ocomp_protocol::state::OcompJobStatus;

use outbe_primitives::OutbeReceipt;

use reth_provider::BlockHashReader;
use reth_provider::BlockIdReader;

use reth_provider::BlockReader;
use reth_provider::ReceiptProvider;

use reth_provider::StateProviderFactory;

use tracing::info;

#[derive(Debug)]
pub(super) enum RetentionReconciliationDispositionV1 {
    ProcessFrame,
    RetryFrame(outbe_node::ocomp::retention::RetentionError),
    Fatal(outbe_node::ocomp::retention::RetentionError),
}

pub(super) fn classify_retention_reconciliation(
    result: Result<(), outbe_node::ocomp::retention::RetentionError>,
    retention_required: bool,
) -> RetentionReconciliationDispositionV1 {
    match result {
        Ok(()) => RetentionReconciliationDispositionV1::ProcessFrame,
        Err(outbe_node::ocomp::retention::RetentionError::RetentionCoordinatorNotInstalled)
            if !retention_required =>
        {
            RetentionReconciliationDispositionV1::ProcessFrame
        }
        Err(
            error @ (outbe_node::ocomp::retention::RetentionError::JournalUnavailable { .. }
            | outbe_node::ocomp::retention::RetentionError::Quarantined(_)
            | outbe_node::ocomp::retention::RetentionError::RegistryCapacity
            | outbe_node::ocomp::retention::RetentionError::RetentionCoordinatorNotInstalled),
        ) => RetentionReconciliationDispositionV1::RetryFrame(error),
        Err(error) => RetentionReconciliationDispositionV1::Fatal(error),
    }
}

pub(super) fn retention_runtime_error_requires_frame_retry(error: &eyre::Report) -> bool {
    matches!(
        error.downcast_ref::<outbe_node::ocomp::retention::RetentionError>(),
        Some(
            outbe_node::ocomp::retention::RetentionError::JournalUnavailable { .. }
                | outbe_node::ocomp::retention::RetentionError::Quarantined(_)
                | outbe_node::ocomp::retention::RetentionError::RegistryCapacity
        )
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
    pub(super) fn reconcile_export_ack(
        &mut self,
        job_id: B256,
        canonical: &OcompJobRecordV1,
        publish_offer: bool,
    ) -> eyre::Result<bool> {
        if self.acknowledged_exports.contains(&job_id) {
            return Ok(true);
        }
        let job = self
            .jobs
            .get(&job_id)
            .ok_or_else(|| eyre::eyre!("embedded OCOMP job disappeared before export"))?;
        let bundle_hash = job.record.spec.summary.protocol_bundle_hash;
        let spool = self.discovery_spools.get(&bundle_hash).ok_or_else(|| {
            eyre::eyre!("OCOMP discovery spool is missing for bundle {bundle_hash}")
        })?;
        let offer = match self.pending_offers.get(&job_id) {
            Some(reference) => reference.clone(),
            None if !publish_offer => DiscoveryOfferRefV1::from_spec(
                self.chain_id,
                self.genesis_hash,
                job.record.generation,
                &job.record.spec,
                &poc_schema_limits(),
            )?,
            None => {
                let (reference, _) = spool.put_offer(job.record.generation, &job.record.spec)?;
                self.pending_offers.insert(job_id, reference.clone());
                info!(
                    %job_id,
                    observation_id = %reference.observation_id,
                    generation = reference.generation,
                    "persisted durable OCOMP discovery offer; awaiting SnapshotExporter ACK"
                );
                reference
            }
        };
        let Some(acknowledgment) = spool.ack(&offer.observation_id)? else {
            return Ok(false);
        };
        if acknowledgment.reference.offer_ref() != offer {
            bail!("OCOMP spool ACK differs from the exact finalized offer");
        }
        self.retention_selector
            .confirm_canonical_export_ack(
                canonical,
                outbe_node::ocomp::retention::ExportAuthorityV1 {
                    source_generation: offer.generation,
                    lease_generation: acknowledgment.lease_generation,
                    manifest_hash: acknowledgment.manifest_hash,
                },
            )
            .wrap_err("commit exact exporter ACK to OCOMP retention")?;
        self.acknowledged_exports.insert(job_id);
        self.pending_offers.remove(&job_id);
        info!(
            %job_id,
            observation_id = %offer.observation_id,
            generation = offer.generation,
            lease_generation = acknowledgment.lease_generation,
            manifest_hash = %acknowledgment.manifest_hash,
            "committed durable SnapshotExporter ACK"
        );
        Ok(true)
    }

    pub(super) fn verify_released_export_ack(
        &self,
        locator: RequestLocatorV1,
        record: &OcompJobRecordV1,
        job_id: B256,
        limits: &outbe_ocomp_protocol::SchemaLimits,
    ) -> eyre::Result<()> {
        let released = self
            .retention_selector
            .released_job_authority(job_id)?
            .ok_or_else(|| eyre::eyre!("closed OCOMP job has no released retention authority"))?;
        let input_lease_id = record
            .intent
            .input_lease_id()
            .wrap_err("derive released OCOMP input lease")?;
        if released.job_id != job_id
            || released.candidate.block_number != locator.block_number
            || released.candidate.block_hash != locator.block_hash
            || released.candidate.state_root != locator.state_root
            || released.candidate.intent_id != locator.intent_id
            || released.candidate.wwd != locator.wwd
            || released.candidate.ce_sealed_root != record.intent.ce_sealed_root
            || released.candidate.protocol_bundle_hash != record.intent.protocol_bundle_hash
            || released.candidate.input_lease_id != input_lease_id
        {
            bail!("released OCOMP retention authority conflicts with canonical typed state");
        }
        let candidate =
            discovery_record(locator, record, job_id, released.source_generation, limits)?;
        let recovered_export =
            if released.export.is_none() && record.status != OcompJobStatus::Expired {
                let spool = self
                    .discovery_spools
                    .get(&candidate.spec.summary.protocol_bundle_hash)
                    .ok_or_else(|| eyre::eyre!("released OCOMP job references an unknown spool"))?;
                let probe = DiscoveryOfferRefV1::from_spec(
                    self.chain_id,
                    self.genesis_hash,
                    released.source_generation,
                    &candidate.spec,
                    limits,
                )?;
                let ack = spool.ack(&probe.observation_id)?.ok_or_else(|| {
                    eyre::eyre!("released OCOMP job has no durable discovery ACK")
                })?;
                if ack.reference.offer_ref() != probe {
                    bail!("released OCOMP ACK differs from its original finalized offer");
                }
                let export = outbe_node::ocomp::retention::ExportAuthorityV1 {
                    source_generation: probe.generation,
                    lease_generation: ack.lease_generation,
                    manifest_hash: ack.manifest_hash,
                };
                self.retention_selector
                    .confirm_canonical_export_ack(record, export)?;
                Some(export)
            } else {
                released.export
            };
        let Some(export) = released_export_authority_for_status(record.status, recovered_export)?
        else {
            return Ok(());
        };
        let bundle_hash = candidate.spec.summary.protocol_bundle_hash;
        let spool = self.discovery_spools.get(&bundle_hash).ok_or_else(|| {
            eyre::eyre!("OCOMP discovery spool is missing for bundle {bundle_hash}")
        })?;
        let probe = DiscoveryOfferRefV1::from_spec(
            self.chain_id,
            self.genesis_hash,
            export.source_generation,
            &candidate.spec,
            limits,
        )?;
        let acknowledgment = spool
            .ack(&probe.observation_id)?
            .ok_or_else(|| eyre::eyre!("released OCOMP export has no durable discovery ACK"))?;
        let reference = acknowledgment.reference.offer_ref();
        if reference != probe
            || acknowledgment.lease_generation != export.lease_generation
            || acknowledgment.manifest_hash != export.manifest_hash
        {
            bail!("released OCOMP ACK conflicts with canonical durable authority");
        }
        Ok(())
    }
}
