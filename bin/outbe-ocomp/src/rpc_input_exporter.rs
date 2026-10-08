//! Builds one authenticated Lysis input manifest from finalized public RPC data.

use std::path::PathBuf;

use alloy_primitives::{keccak256, B256};
use outbe_compressed_entities::{body_commitment, Commitment, ACTIVE_COMMITMENT_SCHEME};
use outbe_node::ocomp::verify_lysis_openings;
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{BuildLysisOpeningsV1, SNAPSHOT_LEASE_WIRE_BYTES},
    input::{
        materialize_authenticated_openings, AuthenticatedOpeningV1, CheckpointIdentityV1,
        InputChunkKind, InputManifestV1,
    },
    intent::{
        intent_storage_key, FinalizedRequestBindingV1, JobIntentV1, VerifiedFinalizedIntentV1,
    },
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    profile::ProtocolBundleV1,
    SchemaLimits, SnapshotExportCommittedV1, SnapshotHandoffV1,
};
use outbe_offchain_data::{ProjectionConfig, ProjectionState};
use outbe_offchain_storage::{
    StorageConfig, StorageError, StorageErrorKind, StorageProvider, StorageReadSource,
};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::RetainedTributePin;
use thiserror::Error;

use crate::{
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    export_receipt::{
        ExportReceiptError, ExportReceiptPreparation, ExportReceiptReader, ExportReceiptStore,
        VerifiedExportReceipt,
    },
    exporter::FinalizedTributeSource,
    input_artifacts::{
        decode_fidelity_subject_key, decode_oracle_subject_key, poc_input_list_limits,
        validate_verified_input_manifest_semantics_observing, DurableInputArtifactPublisher,
        InputArtifactError, InputArtifactIdentity,
    },
    input_inventory::{
        TributeInventoryBuilder, TributeInventoryError, TributeInventoryRecordV1,
        TributeInventorySubjectV1, TributeInventoryWorkConfig,
    },
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    opening_stage::{
        DurableOpeningStage, OpeningResolutionV1, OpeningStageError, OpeningStageSubjectV1,
    },
    public_rpc::PublicOcompRpcClientV1,
    supervisor::DiscoveryRecord,
};

use crate::input_artifacts::{
    ExpectedInputCounts, InputArtifactContext, InputManifestVerification,
    PublishedStreamingInputArtifacts,
};
use crate::input_inventory::SealedTributeInventory;
use crate::opening_stage::OpeningStageReportV1;

// A progress heartbeat, not a capacity limit. The exporter still consumes all
// records and keeps only its existing bounded publisher window in memory.
mod publication;
mod replay;
use publication::ExportWork;
use replay::{verify_replayed_finalized_inputs, ReplayAuthority};

const EXPORT_PROGRESS_RECORD_HEARTBEAT: u64 = 256;

#[derive(Clone, Debug)]
pub struct RpcInputExporterConfigV1 {
    pub rpc_url: String,
    pub rpc_max_response_bytes: usize,
    pub storage: StorageConfig,
    pub tribute_page_limit: usize,
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
    storage_source: StorageReadSource,
    cas: FilesystemCas,
    reader: FilesystemCasReader,
}

impl RpcInputExporterV1 {
    pub fn open(config: RpcInputExporterConfigV1) -> Result<Self, RpcInputExporterErrorV1> {
        let storage_source = StorageProvider::new(config.storage.clone())
            .and_then(|provider| {
                outbe_offchain_data::entity_partition_routing()
                    .map(|routing| provider.with_partition_routing(routing))
            })
            .and_then(|provider| provider.read_source(&hex::encode(config.protocol_bundle_hash)))
            .map_err(source_open_error)?;
        let rpc =
            PublicOcompRpcClientV1::new(config.rpc_url.clone(), config.rpc_max_response_bytes)?;
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
            storage_source,
            cas,
            reader,
        })
    }

    /// Idempotently publishes the exact input manifest and its durable local
    /// receipt. Existing receipts are cold-reloaded and accepted only after the
    /// normal manifest validation succeeds.
    pub fn export(
        &mut self,
        discovery: &DiscoveryRecord,
    ) -> Result<VerifiedExportReceipt, RpcInputExporterErrorV1> {
        self.export_observing(discovery, || {})
    }

    pub fn export_observing(
        &mut self,
        discovery: &DiscoveryRecord,
        on_progress: impl Fn(),
    ) -> Result<VerifiedExportReceipt, RpcInputExporterErrorV1> {
        on_progress();
        let job_id = discovery.spec.summary.job_id;
        let job_key = hex::encode(job_id.as_slice());
        let input_ref_catalog_root = self.config.input_ref_root.join(&job_key);
        let work_root = self.config.input_ref_root.join(".work").join(&job_key);
        // Revalidate the durable discovery binding before accepting even an
        // exact local export replay. A valid old receipt must not make a stale
        // or substituted discovery journal authoritative after restart.
        let finalized = verified_discovery_intent(discovery, &self.config)?;
        let expected_input = ExpectedInputAuthorityV1::from_finalized(
            &finalized,
            self.config.protocol_bundle.bundle(),
            &self.config.limits,
        )?;
        let work = ExportWork {
            discovery,
            finalized: &finalized,
            expected: &expected_input,
            job_id,
            input_ref_catalog_root: &input_ref_catalog_root,
            work_root: &work_root,
        };
        if let Some(receipt) = self.replay_receipt(&work, &on_progress)? {
            return Ok(receipt);
        }

        if finalized.job_id != job_id {
            return Err(RpcInputExporterErrorV1::Authority("discovery JobId"));
        }
        let (inventory, pin) = self.prepare_inventory(&work, &on_progress)?;
        let (published, _publication_readers) =
            self.publish_inventory(&work, &inventory, &on_progress)?;
        if published.tribute_count != finalized.intent.authenticated_day_count
            || published.tribute_nominal_total != finalized.intent.authenticated_day_nominal
        {
            return Err(RpcInputExporterErrorV1::Authority(
                "published Tribute conservation",
            ));
        }

        self.commit_publication(&work, pin, published, &on_progress)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpectedInputAuthorityV1 {
    protocol_bundle_hash: B256,
    job_id: B256,
    attempt: u32,
    checkpoint: CheckpointIdentityV1,
    wwd: u32,
    sealed_tribute_collection_key: B256,
    sealed_tribute_collection_root: B256,
    tribute_count: u32,
    tribute_nominal_total: alloy_primitives::U256,
    body_codec_id: B256,
    opening_codec_registry_hash: B256,
}

impl ExpectedInputAuthorityV1 {
    fn from_finalized(
        finalized: &VerifiedFinalizedIntentV1,
        bundle: &ProtocolBundleV1,
        limits: &SchemaLimits,
    ) -> Result<Self, RpcInputExporterErrorV1> {
        let protocol_bundle_hash = bundle
            .protocol_bundle_hash(limits)
            .map_err(|error| stage("hash protocol bundle", error))?;
        if protocol_bundle_hash != finalized.intent.protocol_bundle_hash {
            return Err(RpcInputExporterErrorV1::Authority(
                "finalized protocol bundle",
            ));
        }
        Ok(Self {
            protocol_bundle_hash,
            job_id: finalized.job_id,
            attempt: finalized.intent.attempt,
            checkpoint: CheckpointIdentityV1 {
                finalized_block_number: finalized.request.block_number,
                finalized_block_hash: finalized.request.block_hash,
                finalized_state_root: finalized.request.state_root,
                finalized_ce_root: finalized.intent.ce_sealed_root,
                ce_schema_version: u16::try_from(
                    outbe_compressed_entities::LOCAL_STORAGE_SCHEMA_VERSION,
                )
                .map_err(|_| RpcInputExporterErrorV1::Authority("CE schema version"))?,
            },
            wwd: finalized.intent.wwd,
            sealed_tribute_collection_key: finalized.intent.sealed_tribute_collection_key,
            sealed_tribute_collection_root: finalized.intent.sealed_tribute_collection_root,
            tribute_count: finalized.intent.authenticated_day_count,
            tribute_nominal_total: finalized.intent.authenticated_day_nominal,
            body_codec_id: bundle.tribute_body_codec_id,
            opening_codec_registry_hash: bundle
                .opening_codec_registry_hash()
                .map_err(|error| stage("hash opening codec registry", error))?,
        })
    }
}

fn require_replayed_input_authority(
    expected: &ExpectedInputAuthorityV1,
    receipt_checkpoint: &CheckpointIdentityV1,
    manifest: &InputManifestV1,
) -> Result<(), RpcInputExporterErrorV1> {
    let checkpoint_and_protocol_mismatch = receipt_checkpoint != &expected.checkpoint
        || manifest.protocol_bundle_hash != expected.protocol_bundle_hash;
    let job_binding_mismatches = manifest.job_id != expected.job_id
        || manifest.attempt != expected.attempt
        || manifest.checkpoint != expected.checkpoint;
    let sealed_population_mismatches = manifest.wwd != expected.wwd
        || manifest.sealed_tribute_collection_key != expected.sealed_tribute_collection_key
        || manifest.sealed_tribute_collection_root != expected.sealed_tribute_collection_root;
    let codec_and_totals_mismatch = (
        manifest.tribute_count,
        manifest.tribute_nominal_total,
        manifest.body_codec_id,
        manifest.opening_codec_registry_hash,
    ) != (
        expected.tribute_count,
        expected.tribute_nominal_total,
        expected.body_codec_id,
        expected.opening_codec_registry_hash,
    );
    if [
        checkpoint_and_protocol_mismatch,
        job_binding_mismatches,
        sealed_population_mismatches,
        codec_and_totals_mismatch,
    ]
    .into_iter()
    .any(|mismatch| mismatch)
    {
        return Err(RpcInputExporterErrorV1::Authority(
            "replayed input manifest finalized binding",
        ));
    }
    Ok(())
}

fn verify_durable_lysis_openings(
    fidelity: &AuthenticatedOpeningV1,
    oracle: &AuthenticatedOpeningV1,
    finalized: &VerifiedFinalizedIntentV1,
    subjects: &OpeningSubjectsV1,
    policy: (&ProtocolBundleV1, &SchemaLimits),
) -> Result<(), OpeningStageError> {
    let (bundle, limits) = policy;
    fidelity
        .validate_against_bundle(bundle, limits)
        .map_err(|error| OpeningStageError::Verification(error.to_string()))?;
    oracle
        .validate_against_bundle(bundle, limits)
        .map_err(|error| OpeningStageError::Verification(error.to_string()))?;
    let fidelity = fidelity
        .decode_and_validate_raw_opening(finalized.request.state_root, limits)
        .map_err(|error| OpeningStageError::Verification(error.to_string()))?;
    let oracle = oracle
        .decode_and_validate_raw_opening(finalized.request.state_root, limits)
        .map_err(|error| OpeningStageError::Verification(error.to_string()))?;
    verify_lysis_openings(
        &LysisOpeningsProofV1 {
            protocol_bundle_hash: finalized.intent.protocol_bundle_hash,
            job_id: finalized.job_id,
            finalized_block_hash: finalized.request.block_hash,
            finalized_state_root: finalized.request.state_root,
            wwd: finalized.intent.wwd,
            subjects: subjects.clone(),
            fidelity,
            oracle,
        },
        finalized,
        subjects,
        limits,
    )
    .map_err(|error| OpeningStageError::Verification(error.to_string()))
}

fn verified_discovery_intent(
    discovery: &DiscoveryRecord,
    config: &RpcInputExporterConfigV1,
) -> Result<VerifiedFinalizedIntentV1, RpcInputExporterErrorV1> {
    let summary = &discovery.spec.summary;
    let intent =
        JobIntentV1::decode_canonical(&discovery.spec.canonical_job_intent.0, &config.limits)
            .map_err(|error| stage("decode finalized JobIntent", error))?;
    let intent_id = intent
        .intent_id(&config.limits)
        .map_err(|error| stage("hash finalized JobIntent", error))?;
    let job_id = intent
        .job_id(
            summary.finalized_block_hash,
            summary.finalized_state_root,
            &config.limits,
        )
        .map_err(|error| stage("derive finalized JobId", error))?;
    let discovery_mismatch = discovery.cursor != summary.cursor
        || intent_id != summary.intent_id
        || job_id != summary.job_id;
    let network_mismatch = intent.chain_id != config.chain_id
        || intent.genesis_hash != config.genesis_hash
        || intent.fork_id != config.fork_id;
    let protocol_mismatch = intent.protocol_bundle_hash != config.protocol_bundle_hash
        || summary.protocol_bundle_hash != config.protocol_bundle_hash;
    if discovery_mismatch || network_mismatch || protocol_mismatch {
        return Err(RpcInputExporterErrorV1::Authority(
            "finalized discovery binding",
        ));
    }
    Ok(VerifiedFinalizedIntentV1 {
        intent,
        intent_id,
        intent_storage_key: intent_storage_key(intent_id)
            .map_err(|error| stage("derive finalized intent storage key", error))?,
        job_id,
        request: FinalizedRequestBindingV1 {
            block_number: summary.cursor,
            block_hash: summary.finalized_block_hash,
            state_root: summary.finalized_state_root,
        },
    })
}

fn require_projection_checkpoint(
    state: Option<&ProjectionState>,
    request: &FinalizedRequestBindingV1,
) -> Result<(), RpcInputExporterErrorV1> {
    let checkpoint =
        state
            .and_then(|state| state.checkpoint)
            .ok_or(RpcInputExporterErrorV1::Authority(
                "projection checkpoint missing",
            ))?;
    if checkpoint.block_number < request.block_number
        || (checkpoint.block_number == request.block_number
            && checkpoint.block_hash != request.block_hash)
    {
        return Err(RpcInputExporterErrorV1::Authority(
            "projection checkpoint does not cover finalized request",
        ));
    }
    Ok(())
}

fn committed_pin_generation(source_generation: u64) -> Result<u64, RpcInputExporterErrorV1> {
    source_generation
        .checked_add(1)
        .ok_or(RpcInputExporterErrorV1::Authority(
            "discovery generation overflow",
        ))
}

fn require_receipt_generation(
    receipt: &VerifiedExportReceipt,
    source_generation: u64,
) -> Result<(), RpcInputExporterErrorV1> {
    if receipt.source_pin_generation() != source_generation
        || receipt.committed().pin_generation != committed_pin_generation(source_generation)?
    {
        return Err(RpcInputExporterErrorV1::Authority(
            "export receipt discovery generation",
        ));
    }
    Ok(())
}

fn is_lysis_opening_capacity_error(error: &impl std::fmt::Display) -> bool {
    error
        .to_string()
        .contains("Lysis opening bytes exceeds cap: ")
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

fn source_open_error(error: StorageError) -> RpcInputExporterErrorV1 {
    match error.kind() {
        StorageErrorKind::Unavailable | StorageErrorKind::RequestDeadline => {
            RpcInputExporterErrorV1::SourceStorageUnavailable
        }
        _ => stage("open finalized Tribute source", error),
    }
}

#[derive(Debug, Error)]
pub enum RpcInputExporterErrorV1 {
    #[error(transparent)]
    Rpc(#[from] crate::public_rpc::PublicRpcError),
    #[error("OCOMP public input authority mismatch: {0}")]
    Authority(&'static str),
    #[error("OCOMP finalized Tribute source storage is unavailable during startup")]
    SourceStorageUnavailable,
    #[error("OCOMP public input stage `{stage}` failed: {detail}")]
    Stage { stage: &'static str, detail: String },
}

impl RpcInputExporterErrorV1 {
    #[must_use]
    pub const fn is_retryable_startup(&self) -> bool {
        matches!(self, Self::SourceStorageUnavailable)
    }
}

#[cfg(test)]
mod tests;
