//! Production reconciliation loop for the fixed snapshot-exporter role.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use alloy_primitives::{B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    AuthenticatedExportView, CeMdbxReadOnly, EnvironmentIdentity, ExportLeaseOffer,
};
use outbe_node::ocomp::verify_lysis_openings;
use outbe_ocomp_protocol::{
    input::materialize_authenticated_openings, intent::ExpectedFinalizedIntentBindingV1,
    opening::partition_lysis_opening_subjects, CommitSnapshotExportV1, RenewSnapshotLeaseV1,
    SchemaLimits, SnapshotHandoffV1,
};
use outbe_offchain_data::ProjectionConfig;
use outbe_offchain_storage::{MongoStorage, MongoStorageConfig};
use outbe_tribute::RetainedTributePin;
use thiserror::Error;

use crate::{
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    export_receipt::{
        list_prepared_export_jobs, ExportReceiptError, ExportReceiptPreparation,
        ExportReceiptStore, VerifiedExportReceipt,
    },
    exporter::FinalizedTributeSource,
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    snapshot_client::{SnapshotExporterNodeClient, SnapshotExporterNodeConfig},
};

#[derive(Clone, Debug)]
pub struct SnapshotExporterConfig {
    pub node: SnapshotExporterNodeConfig,
    pub ce_datadir: PathBuf,
    pub ce_environment: EnvironmentIdentity,
    pub mongo: MongoStorageConfig,
    pub cas_root: PathBuf,
    pub cas_limits: CasLimits,
    pub input_ref_root: PathBuf,
    pub receipt_root: PathBuf,
    pub protocol_bundle: PinnedProtocolBundle,
    pub tribute_page_limit: usize,
    /// Bounds only crash-incomplete `Prepared` journals scanned during recovery.
    /// Completed jobs and the total Tribute population do not consume this cap.
    pub max_recoverable_prepared_jobs: usize,
    pub projection_start_block: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotExportCompleted {
    pub receipt: VerifiedExportReceipt,
    pub canonical_open_ack: Vec<u8>,
    pub collection_root: B256,
    pub tribute_count: u32,
    pub tribute_nominal_total: U256,
    pub ordered_tribute_ids: Vec<String>,
    pub fidelity_slot_count: usize,
    pub oracle_slot_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotExporterReconcile {
    pub next_lease_generation: u64,
    pub replayed_job_ids: Vec<B256>,
    pub completed: Vec<SnapshotExportCompleted>,
}

pub struct SnapshotExporter {
    config: SnapshotExporterConfig,
    limits: SchemaLimits,
    node: SnapshotExporterNodeClient,
    cas: FilesystemCas,
    reader: FilesystemCasReader,
    tribute_source: FinalizedTributeSource,
}

impl SnapshotExporter {
    pub fn open(config: SnapshotExporterConfig) -> Result<Self, SnapshotExporterError> {
        require(
            config.node.identity.protocol_bundle_hash == config.protocol_bundle.hash(),
            "endpoint protocol bundle hash",
        )?;
        require(
            config.node.identity.chain_id == config.ce_environment.chain_id
                && config.node.identity.genesis_hash == config.ce_environment.genesis_hash,
            "CE environment chain identity",
        )?;
        require(
            config.max_recoverable_prepared_jobs != 0 && config.projection_start_block != 0,
            "exporter reconciliation bounds",
        )?;
        let limits = config.node.limits;
        let node = stage(
            "connect node snapshot endpoint",
            SnapshotExporterNodeClient::connect(&config.node),
        )?;
        let cas = stage(
            "open snapshot-exporter CAS",
            FilesystemCas::open(
                &config.cas_root,
                CasWriterRole::SnapshotExporter,
                config.cas_limits,
            ),
        )?;
        let reader = stage(
            "open snapshot-exporter CAS reader",
            FilesystemCasReader::open(&config.cas_root, config.cas_limits),
        )?;
        let storage = Arc::new(stage(
            "connect Mongo projection",
            MongoStorage::connect(config.mongo.clone()),
        )?);
        let tribute_source = stage(
            "open finalized Tribute source",
            FinalizedTributeSource::new(storage, config.tribute_page_limit),
        )?;
        Ok(Self {
            config,
            limits,
            node,
            cas,
            reader,
            tribute_source,
        })
    }

    pub fn reconcile_once(
        &mut self,
        after_lease_generation: u64,
    ) -> Result<SnapshotExporterReconcile, SnapshotExporterError> {
        let replayed_job_ids = self.replay_prepared_exports()?;
        let listing = stage(
            "list node snapshot handoffs",
            self.node.list(after_lease_generation),
        )?;
        let mut completed = Vec::new();
        for listed in listing.handoffs {
            let handoff = stage(
                "get exact node snapshot handoff",
                self.node.get(listed.job_id, listed.lease_generation),
            )?;
            require(handoff == listed, "listed snapshot handoff")?;
            if replayed_job_ids.binary_search(&handoff.job_id).is_ok() {
                continue;
            }
            completed.push(self.export_handoff(handoff)?);
        }
        Ok(SnapshotExporterReconcile {
            next_lease_generation: listing.next_lease_generation,
            replayed_job_ids,
            completed,
        })
    }

    fn replay_prepared_exports(&mut self) -> Result<Vec<B256>, SnapshotExporterError> {
        let jobs = stage(
            "discover prepared export jobs",
            list_prepared_export_jobs(
                &self.config.receipt_root,
                self.config.max_recoverable_prepared_jobs,
            ),
        )?;
        for job_id in &jobs {
            let mut store = stage(
                "open prepared export journal",
                ExportReceiptStore::open(&self.config.receipt_root, *job_id, self.limits),
            )?;
            match store.load_exact(&self.reader) {
                Ok(receipt) => {
                    let replayed = stage(
                        "replay completed node export commit",
                        self.node.commit(receipt.commit_replay_request()),
                    )?;
                    stage(
                        "verify completed node export replay",
                        receipt.require_exact_node_replay(&replayed),
                    )?;
                }
                Err(ExportReceiptError::MissingReceipt) => {
                    let prepared = stage(
                        "reload prepared export",
                        store.load_prepared_exact(&self.reader),
                    )?;
                    let committed = stage(
                        "recover node export commit",
                        self.node.commit(prepared.commit_request()),
                    )?;
                    let _ = stage(
                        "record recovered node export commit",
                        store.record_committed(&self.cas, &self.reader, &prepared, &committed),
                    )?;
                }
                Err(error) => {
                    return Err(SnapshotExporterError::Stage {
                        stage: "reload completed export receipt",
                        detail: error.to_string(),
                    });
                }
            }
        }
        Ok(jobs)
    }

    fn export_handoff(
        &mut self,
        handoff: SnapshotHandoffV1,
    ) -> Result<SnapshotExportCompleted, SnapshotExporterError> {
        let verified = stage(
            "authenticate finalized JobIntent",
            self.node.authenticate_handoff(
                &handoff,
                ExpectedFinalizedIntentBindingV1 {
                    chain_id: self.config.node.identity.chain_id,
                    genesis_hash: self.config.node.identity.genesis_hash,
                    fork_id: self.config.protocol_bundle.bundle().fork_id,
                    protocol_bundle_hash: self.config.protocol_bundle.hash(),
                },
            ),
        )?;
        let day = WorldwideDay::new(verified.intent.wwd);
        let offer = stage(
            "decode node snapshot lease",
            ExportLeaseOffer::decode_fixed(&handoff.canonical_lease_offer.0),
        )?;
        require(
            offer.generation() == handoff.lease_generation
                && offer.identity().block_number == handoff.checkpoint.finalized_block_number
                && offer.identity().block_hash == handoff.checkpoint.finalized_block_hash
                && offer.identity().root == handoff.checkpoint.finalized_ce_root,
            "snapshot lease identity",
        )?;
        let store = stage(
            "open production read-only CE",
            CeMdbxReadOnly::open(&self.config.ce_datadir, self.config.ce_environment.clone()),
        )?;
        let catalog = stage(
            "open exact finalized CE snapshot",
            store.open_exact(offer.identity()),
        )?;
        let acknowledgement = stage("confirm exact CE open", offer.confirm_open(&catalog))?;
        let opened = stage(
            "acknowledge exact CE open",
            self.node.acknowledge_open(RenewSnapshotLeaseV1 {
                job_id: handoff.job_id,
                lease_generation: handoff.lease_generation,
                canonical_open_ack: outbe_ocomp_protocol::common::BoundedBytes(
                    acknowledgement.encode_fixed().to_vec(),
                ),
            }),
        )?;
        require(
            opened.job_id == handoff.job_id && opened.lease_generation == handoff.lease_generation,
            "opened snapshot lease",
        )?;

        let export = stage(
            "open authenticated CE export view",
            AuthenticatedExportView::new(catalog),
        )?;
        let closure = stage(
            "close exact Tribute partition",
            export.close_tribute_partition(
                day,
                verified.intent.sealed_tribute_collection_key,
                verified.intent.sealed_tribute_collection_root,
                verified.intent.authenticated_day_count,
            ),
        )?;
        let projection = stage(
            "read Mongo projection checkpoint",
            self.tribute_source.projection_state(ProjectionConfig {
                chain_id: self.config.node.identity.chain_id,
                genesis_hash: self.config.node.identity.genesis_hash,
                start_block: self.config.projection_start_block,
            }),
        )?
        .ok_or(SnapshotExporterError::MissingProjectionCheckpoint)?
        .checkpoint
        .ok_or(SnapshotExporterError::MissingProjectionCheckpoint)?;
        let contained = stage(
            "prove Mongo projection containment",
            self.node.require_projection_contains(
                &handoff,
                projection.block_number,
                projection.block_hash,
            ),
        )?;
        require(
            contained.job_id == handoff.job_id
                && contained.lease_generation == handoff.lease_generation,
            "projection containment response",
        )?;

        let mut stream = stage(
            "open authenticated Tribute body stream",
            self.tribute_source.stream(
                RetainedTributePin {
                    job_id: handoff.job_id,
                    worldwide_day: day,
                },
                &closure,
                verified.intent.authenticated_day_nominal,
            ),
        )?;
        let mut ordered_tribute_ids = Vec::new();
        let mut canonical_tributes = Vec::new();
        let mut owners = BTreeSet::new();
        let mut settlement_isos = BTreeSet::new();
        settlement_isos.insert(840);
        while let Some(record) = stage("read authenticated Tribute body", stream.next_record())? {
            ordered_tribute_ids.push(hex::encode(record.tribute_id.as_bytes()));
            owners.insert(record.body.owner);
            settlement_isos.insert(record.body.reference_currency);
            canonical_tributes.push(record.canonical_body);
        }
        let summary = stage("close Tribute body stream", stream.finish())?;

        let owner_set = owners.into_iter().collect::<Vec<_>>();
        let iso_set = settlement_isos.into_iter().collect::<Vec<_>>();
        let subjects = stage(
            "partition bounded Lysis opening subjects",
            partition_lysis_opening_subjects(&owner_set, &iso_set, &self.limits),
        )?;
        let mut fidelity_openings = Vec::new();
        let mut oracle_opening = None;
        let mut fidelity_slot_count = 0_usize;
        let mut oracle_slot_count = 0_usize;
        for subject_batch in &subjects {
            let openings = stage(
                "request finalized Lysis openings",
                self.node
                    .lysis_openings(handoff.job_id, subject_batch.clone()),
            )?;
            stage(
                "verify finalized Lysis openings",
                verify_lysis_openings(&openings, &verified, subject_batch, &self.limits),
            )?;
            fidelity_slot_count = fidelity_slot_count
                .checked_add(openings.fidelity.ordered_slots.len())
                .ok_or(SnapshotExporterError::IntegerOverflow)?;
            oracle_slot_count = openings.oracle.ordered_slots.len();
            let materialized = stage(
                "materialize authenticated Lysis openings",
                materialize_authenticated_openings(
                    &openings,
                    self.config.protocol_bundle.bundle(),
                    &self.limits,
                ),
            )?;
            fidelity_openings.push(materialized.fidelity);
            match &oracle_opening {
                None => oracle_opening = Some(materialized.oracle),
                Some(existing) => require(
                    existing == &materialized.oracle,
                    "canonical Oracle opening across bounded batches",
                )?,
            }
        }
        let published = stage(
            "publish complete input artifact set",
            publish_input_artifact_set(
                &self.cas,
                self.config
                    .input_ref_root
                    .join(hex::encode(handoff.job_id.as_slice())),
                self.config.protocol_bundle.bundle(),
                InputArtifactContents {
                    identity: InputArtifactIdentity {
                        job_id: handoff.job_id,
                        attempt: verified.intent.attempt,
                        checkpoint: handoff.checkpoint.clone(),
                        wwd: verified.intent.wwd,
                        sealed_tribute_collection_key: verified
                            .intent
                            .sealed_tribute_collection_key,
                        sealed_tribute_collection_root: verified
                            .intent
                            .sealed_tribute_collection_root,
                    },
                    canonical_tributes,
                    fidelity_openings,
                    oracle_opening: oracle_opening
                        .ok_or(SnapshotExporterError::MissingOracleOpening)?,
                },
                &self.limits,
                poc_input_list_limits(),
            ),
        )?;
        require(
            published.tribute_count == summary.record_count
                && published.tribute_nominal_total == summary.nominal_total,
            "published Tribute conservation",
        )?;

        let mut receipt_store = stage(
            "open exporter receipt journal",
            ExportReceiptStore::open(&self.config.receipt_root, handoff.job_id, self.limits),
        )?;
        let (_, prepared) = stage(
            "durably prepare node export commit",
            receipt_store.prepare(
                &self.cas,
                &self.reader,
                ExportReceiptPreparation {
                    handoff: &handoff,
                    manifest_ref: &published.manifest_ref,
                    manifest_hash: published.manifest_hash,
                },
            ),
        )?;
        let committed = stage(
            "commit manifest hash to node-owned finalized pin",
            self.node.commit(CommitSnapshotExportV1 {
                job_id: handoff.job_id,
                pin_generation: handoff.pin_generation,
                lease_generation: handoff.lease_generation,
                manifest_hash: published.manifest_hash,
            }),
        )?;
        let (_, receipt) = stage(
            "record node-committed export receipt",
            receipt_store.record_committed(&self.cas, &self.reader, &prepared, &committed),
        )?;
        Ok(SnapshotExportCompleted {
            receipt,
            canonical_open_ack: acknowledgement.encode_fixed().to_vec(),
            collection_root: closure.collection_root,
            tribute_count: summary.record_count,
            tribute_nominal_total: summary.nominal_total,
            ordered_tribute_ids,
            fidelity_slot_count,
            oracle_slot_count,
        })
    }
}

fn require(condition: bool, what: &'static str) -> Result<(), SnapshotExporterError> {
    if condition {
        Ok(())
    } else {
        Err(SnapshotExporterError::AuthorityMismatch(what))
    }
}

fn stage<T, E: std::fmt::Display>(
    stage: &'static str,
    result: Result<T, E>,
) -> Result<T, SnapshotExporterError> {
    result.map_err(|error| SnapshotExporterError::Stage {
        stage,
        detail: error.to_string(),
    })
}

#[derive(Debug, Error)]
pub enum SnapshotExporterError {
    #[error("snapshot exporter authority mismatch: {0}")]
    AuthorityMismatch(&'static str),
    #[error("snapshot exporter stage `{stage}` failed: {detail}")]
    Stage { stage: &'static str, detail: String },
    #[error("snapshot exporter Mongo projection checkpoint is missing")]
    MissingProjectionCheckpoint,
    #[error("snapshot exporter did not receive a canonical Oracle opening")]
    MissingOracleOpening,
    #[error("snapshot exporter integer overflow")]
    IntegerOverflow,
}
