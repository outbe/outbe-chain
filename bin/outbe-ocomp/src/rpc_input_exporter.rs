//! Builds one authenticated Lysis input manifest from finalized public RPC data.

use std::{collections::BTreeSet, path::PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_node::ocomp::{
    build_public_lysis_openings, finality::PublicFinalizedIntentProofBuilderV1,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::SNAPSHOT_LEASE_WIRE_BYTES,
    input::{materialize_authenticated_openings, CheckpointIdentityV1},
    intent::ExpectedFinalizedIntentBindingV1,
    opening::partition_lysis_opening_subjects,
    SchemaLimits, SnapshotExportCommittedV1, SnapshotHandoffV1,
};
use thiserror::Error;

use crate::{
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    export_receipt::{ExportReceiptPreparation, ExportReceiptReader, ExportReceiptStore},
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    public_rpc::PublicOcompRpcClientV1,
    rpc_projection::{RpcFinalizedProjectionV1, RpcProjectionConfigV1},
    supervisor::DiscoveryRecord,
};

#[derive(Clone, Debug)]
pub struct RpcInputExporterConfigV1 {
    pub rpc_url: String,
    pub rpc_max_response_bytes: usize,
    pub projection: RpcProjectionConfigV1,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub fork_id: B256,
    pub protocol_bundle_hash: B256,
    pub cas_root: PathBuf,
    pub cas_limits: CasLimits,
    pub input_ref_root: PathBuf,
    pub receipt_root: PathBuf,
    pub protocol_bundle: PinnedProtocolBundle,
    pub limits: SchemaLimits,
}

pub struct RpcInputExporterV1 {
    config: RpcInputExporterConfigV1,
    rpc: PublicOcompRpcClientV1,
    projection: RpcFinalizedProjectionV1,
    cas: FilesystemCas,
    reader: FilesystemCasReader,
}

impl RpcInputExporterV1 {
    pub fn open(config: RpcInputExporterConfigV1) -> Result<Self, RpcInputExporterErrorV1> {
        let rpc =
            PublicOcompRpcClientV1::new(config.rpc_url.clone(), config.rpc_max_response_bytes)?;
        let projection = RpcFinalizedProjectionV1::open(config.projection.clone())
            .map_err(|error| stage("open finalized receipt projection", error))?;
        let cas = FilesystemCas::open(
            &config.cas_root,
            CasWriterRole::SnapshotExporter,
            config.cas_limits,
        )
        .map_err(|error| stage("open input CAS", error))?;
        let reader = FilesystemCasReader::open(&config.cas_root, config.cas_limits)
            .map_err(|error| stage("open input CAS reader", error))?;
        Ok(Self {
            config,
            rpc,
            projection,
            cas,
            reader,
        })
    }

    /// Idempotently publishes the exact input manifest and its durable local
    /// receipt. Existing receipts are cold-reloaded and accepted only after the
    /// normal manifest validation succeeds.
    pub fn export(&mut self, discovery: &DiscoveryRecord) -> Result<(), RpcInputExporterErrorV1> {
        let job_id = discovery.spec.summary.job_id;
        if let Some(reader) =
            ExportReceiptReader::try_open(&self.config.receipt_root, job_id, self.config.limits)
                .map_err(|error| stage("inspect input receipt", error))?
        {
            reader
                .load_exact(&self.reader)
                .map_err(|error| stage("reload input receipt", error))?;
            return Ok(());
        }

        let (_, finalized) =
            PublicFinalizedIntentProofBuilderV1::new(&self.rpc, self.config.limits)
                .build_and_verify(
                    discovery.cursor,
                    discovery.spec.summary.intent_id,
                    ExpectedFinalizedIntentBindingV1 {
                        chain_id: self.config.chain_id,
                        genesis_hash: self.config.genesis_hash,
                        fork_id: self.config.fork_id,
                        protocol_bundle_hash: self.config.protocol_bundle_hash,
                    },
                )
                .map_err(|error| stage("verify finalized input authority", error))?;
        if finalized.job_id != job_id {
            return Err(RpcInputExporterErrorV1::Authority("discovery JobId"));
        }
        let pin = self
            .projection
            .install_retention_pin(&finalized)
            .map_err(|error| stage("install Tribute retention pin", error))?;
        let projected = self
            .projection
            .project_through(finalized.request.block_number)
            .map_err(|error| stage("project finalized receipts", error))?;
        if projected < finalized.request.block_number {
            return Err(RpcInputExporterErrorV1::Authority("projection checkpoint"));
        }

        let partition = self
            .projection
            .tribute_source()
            .reconstruct_partition(
                pin,
                finalized.intent.sealed_tribute_collection_root,
                finalized.intent.authenticated_day_count,
                finalized.intent.authenticated_day_nominal,
                self.config.limits.max_collection_items,
            )
            .map_err(|error| stage("reconstruct sealed Tribute partition", error))?;
        let mut owners = BTreeSet::new();
        let mut isos = BTreeSet::from([840_u16]);
        let canonical_tributes = partition
            .records
            .into_iter()
            .map(|record| {
                owners.insert(record.body.owner);
                isos.insert(record.body.reference_currency);
                record.canonical_body
            })
            .collect::<Vec<_>>();
        let subjects = partition_lysis_opening_subjects(
            &owners.into_iter().collect::<Vec<_>>(),
            &isos.into_iter().collect::<Vec<_>>(),
            &self.config.limits,
        )
        .map_err(|error| stage("partition Lysis opening subjects", error))?;
        let mut fidelity_openings = Vec::new();
        let mut oracle_opening = None;
        for subject_batch in subjects {
            let openings = build_public_lysis_openings(
                &self.rpc,
                &finalized,
                subject_batch,
                &self.config.limits,
            )
            .map_err(|error| stage("build public Lysis openings", error))?;
            let materialized = materialize_authenticated_openings(
                &openings,
                self.config.protocol_bundle.bundle(),
                &self.config.limits,
            )
            .map_err(|error| stage("materialize Lysis openings", error))?;
            fidelity_openings.push(materialized.fidelity);
            match &oracle_opening {
                None => oracle_opening = Some(materialized.oracle),
                Some(existing) if existing == &materialized.oracle => {}
                Some(_) => return Err(RpcInputExporterErrorV1::Authority("Oracle opening replay")),
            }
        }
        let checkpoint = CheckpointIdentityV1 {
            finalized_block_number: finalized.request.block_number,
            finalized_block_hash: finalized.request.block_hash,
            finalized_state_root: finalized.request.state_root,
            finalized_ce_root: finalized.intent.ce_sealed_root,
            ce_schema_version: u16::try_from(
                outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
            )
            .map_err(|_| RpcInputExporterErrorV1::Authority("CE schema version"))?,
        };
        let published = publish_input_artifact_set(
            &self.cas,
            self.config
                .input_ref_root
                .join(hex::encode(job_id.as_slice())),
            self.config.protocol_bundle.bundle(),
            InputArtifactContents {
                identity: InputArtifactIdentity {
                    job_id,
                    attempt: finalized.intent.attempt,
                    checkpoint: checkpoint.clone(),
                    wwd: finalized.intent.wwd,
                    sealed_tribute_collection_key: finalized.intent.sealed_tribute_collection_key,
                    sealed_tribute_collection_root: finalized.intent.sealed_tribute_collection_root,
                },
                canonical_tributes,
                fidelity_openings,
                oracle_opening: oracle_opening
                    .ok_or(RpcInputExporterErrorV1::Authority("Oracle opening"))?,
            },
            &self.config.limits,
            poc_input_list_limits(),
        )
        .map_err(|error| stage("publish input artifacts", error))?;
        if published.tribute_count != finalized.intent.authenticated_day_count
            || published.tribute_nominal_total != finalized.intent.authenticated_day_nominal
        {
            return Err(RpcInputExporterErrorV1::Authority(
                "published Tribute conservation",
            ));
        }

        // This is an OCOMP-local publication record, not a node acknowledgement.
        // The legacy envelope remains versioned for crash-safe compatibility.
        let handoff = SnapshotHandoffV1 {
            job_id,
            input_lease_id: pin.input_lease_id,
            pin_generation: 1,
            lease_generation: 1,
            checkpoint,
            canonical_lease_offer: BoundedBytes(local_publication_lease(job_id)),
        };
        let mut receipt_store =
            ExportReceiptStore::open(&self.config.receipt_root, job_id, self.config.limits)
                .map_err(|error| stage("open input receipt", error))?;
        let (_, prepared) = receipt_store
            .prepare(
                &self.cas,
                &self.reader,
                ExportReceiptPreparation {
                    handoff: &handoff,
                    manifest_ref: &published.manifest_ref,
                    manifest_hash: published.manifest_hash,
                },
            )
            .map_err(|error| stage("prepare input receipt", error))?;
        let committed = SnapshotExportCommittedV1 {
            job_id,
            pin_generation: 2,
            record_hash: keccak256(
                [
                    b"OCOMP_RPC_INPUT_COMMIT_V1".as_slice(),
                    job_id.as_slice(),
                    published.manifest_hash.as_slice(),
                ]
                .concat(),
            ),
        };
        receipt_store
            .record_committed(&self.cas, &self.reader, &prepared, &committed)
            .map_err(|error| stage("commit input receipt", error))?;
        Ok(())
    }
}

fn local_publication_lease(job_id: B256) -> Vec<u8> {
    let digest = keccak256([b"OCOMP_RPC_INPUT_V1".as_slice(), job_id.as_slice()].concat());
    let mut lease = Vec::with_capacity(SNAPSHOT_LEASE_WIRE_BYTES);
    while lease.len() < SNAPSHOT_LEASE_WIRE_BYTES {
        lease.extend_from_slice(digest.as_slice());
    }
    lease.truncate(SNAPSHOT_LEASE_WIRE_BYTES);
    lease
}

fn stage(stage: &'static str, error: impl std::fmt::Display) -> RpcInputExporterErrorV1 {
    RpcInputExporterErrorV1::Stage {
        stage,
        detail: error.to_string(),
    }
}

#[derive(Debug, Error)]
pub enum RpcInputExporterErrorV1 {
    #[error(transparent)]
    Rpc(#[from] crate::public_rpc::PublicRpcError),
    #[error("OCOMP public input authority mismatch: {0}")]
    Authority(&'static str),
    #[error("OCOMP public input stage `{stage}` failed: {detail}")]
    Stage { stage: &'static str, detail: String },
}
