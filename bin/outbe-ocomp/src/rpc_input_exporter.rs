//! Builds one authenticated Lysis input manifest from finalized public RPC data.

use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use alloy_primitives::{keccak256, B256};
use outbe_node::ocomp::verify_lysis_openings;
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{BuildLysisOpeningsV1, SNAPSHOT_LEASE_WIRE_BYTES},
    input::{materialize_authenticated_openings, CheckpointIdentityV1},
    intent::{
        intent_storage_key, FinalizedRequestBindingV1, JobIntentV1, VerifiedFinalizedIntentV1,
    },
    opening::{partition_lysis_opening_subjects, LysisOpeningsProofV1, OpeningSubjectsV1},
    SchemaLimits, SnapshotExportCommittedV1, SnapshotHandoffV1,
};
use thiserror::Error;

use crate::{
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    export_receipt::{
        ExportReceiptError, ExportReceiptPreparation, ExportReceiptReader, ExportReceiptStore,
    },
    input_artifacts::{
        poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
        InputArtifactIdentity,
    },
    public_rpc::PublicOcompRpcClientV1,
    rpc_projection::{RpcFinalizedProjectionV1, RpcProjectionConfigV1, RpcProjectionErrorV1},
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
    projection: Arc<Mutex<RpcFinalizedProjectionV1>>,
    cas: FilesystemCas,
    reader: FilesystemCasReader,
}

impl RpcInputExporterV1 {
    pub fn open(config: RpcInputExporterConfigV1) -> Result<Self, RpcInputExporterErrorV1> {
        let projection = RpcFinalizedProjectionV1::open(config.projection.clone())
            .map_err(projection_open_error)?;
        Self::open_with_shared_projection(config, Arc::new(Mutex::new(projection)))
    }

    /// Opens one bundle-pinned exporter lane over the process-owned finalized
    /// projection. Sharing this handle is what lets active and retiring bundle
    /// lanes coexist without competing for the Mongo writer lease.
    pub fn open_with_shared_projection(
        config: RpcInputExporterConfigV1,
        projection: Arc<Mutex<RpcFinalizedProjectionV1>>,
    ) -> Result<Self, RpcInputExporterErrorV1> {
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
        // Revalidate the durable discovery binding before accepting even an
        // exact local export replay. A valid old receipt must not make a stale
        // or substituted discovery journal authoritative after restart.
        let finalized = verified_discovery_intent(discovery, &self.config)?;
        if let Some(reader) =
            ExportReceiptReader::try_open(&self.config.receipt_root, job_id, self.config.limits)
                .map_err(|error| stage("inspect input receipt", error))?
        {
            match reader.load_exact(&self.reader) {
                Ok(_) => return Ok(()),
                Err(
                    ExportReceiptError::MissingPreparation | ExportReceiptError::MissingReceipt,
                ) => {}
                Err(error) => return Err(stage("reload input receipt", error)),
            }
        }

        if finalized.job_id != job_id {
            return Err(RpcInputExporterErrorV1::Authority("discovery JobId"));
        }
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| RpcInputExporterErrorV1::Authority("projection lock"))?;
        let pin = projection
            .install_retention_pin(&finalized)
            .map_err(|error| stage("install Tribute retention pin", error))?;
        let projected = projection
            .project_through(finalized.request.block_number)
            .map_err(|error| stage("project finalized receipts", error))?;
        if projected < finalized.request.block_number {
            return Err(RpcInputExporterErrorV1::Authority("projection checkpoint"));
        }

        let partition = projection
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
        let mut pending_subjects = VecDeque::from(subjects);
        while let Some(subject_batch) = pending_subjects.pop_front() {
            let canonical_request = BuildLysisOpeningsV1 {
                job_id,
                subjects: subject_batch.clone(),
            }
            .encode_body(&self.config.limits)
            .map_err(|error| stage("encode Lysis openings request", error))?;
            let openings = match self
                .rpc
                .lysis_openings(finalized.intent_id, &canonical_request)
                .and_then(|encoded| {
                    LysisOpeningsProofV1::decode_body(&encoded, &self.config.limits).map_err(
                        |error| crate::public_rpc::PublicRpcError::Malformed {
                            method: "outbe_getOcompLysisOpeningsV1",
                            detail: error.to_string(),
                        },
                    )
                }) {
                Ok(openings) => {
                    verify_lysis_openings(
                        &openings,
                        &finalized,
                        &subject_batch,
                        &self.config.limits,
                    )
                    .map_err(|error| stage("verify node Lysis openings", error))?;
                    openings
                }
                Err(error) if is_lysis_opening_capacity_error(&error) => {
                    let Some((left, right)) = bisect_opening_subjects(subject_batch) else {
                        return Err(stage("build public Lysis openings", error));
                    };
                    // Preserve the canonical owner order while processing the
                    // left half first.
                    pending_subjects.push_front(right);
                    pending_subjects.push_front(left);
                    continue;
                }
                Err(error) => return Err(stage("build public Lysis openings", error)),
            };
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
    if discovery.cursor != summary.cursor
        || intent_id != summary.intent_id
        || job_id != summary.job_id
        || intent.chain_id != config.chain_id
        || intent.genesis_hash != config.genesis_hash
        || intent.fork_id != config.fork_id
        || intent.protocol_bundle_hash != config.protocol_bundle_hash
        || summary.protocol_bundle_hash != config.protocol_bundle_hash
    {
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

fn is_lysis_opening_capacity_error(error: &impl std::fmt::Display) -> bool {
    error
        .to_string()
        .contains("Lysis opening bytes exceeds cap: ")
}

fn bisect_opening_subjects(
    mut subjects: OpeningSubjectsV1,
) -> Option<(OpeningSubjectsV1, OpeningSubjectsV1)> {
    let midpoint = subjects.owners.len() / 2;
    if midpoint == 0 {
        return None;
    }
    let right_owners = subjects.owners.split_off(midpoint);
    let right = OpeningSubjectsV1 {
        owners: right_owners,
        reference_isos: subjects.reference_isos.clone(),
    };
    Some((subjects, right))
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

fn projection_open_error(error: RpcProjectionErrorV1) -> RpcInputExporterErrorV1 {
    match error {
        RpcProjectionErrorV1::StorageUnavailable => {
            RpcInputExporterErrorV1::ProjectionStorageUnavailable
        }
        error => stage("open finalized receipt projection", error),
    }
}

#[derive(Debug, Error)]
pub enum RpcInputExporterErrorV1 {
    #[error(transparent)]
    Rpc(#[from] crate::public_rpc::PublicRpcError),
    #[error("OCOMP public input authority mismatch: {0}")]
    Authority(&'static str),
    #[error("OCOMP finalized receipt projection storage is unavailable during startup")]
    ProjectionStorageUnavailable,
    #[error("OCOMP public input stage `{stage}` failed: {detail}")]
    Stage { stage: &'static str, detail: String },
}

impl RpcInputExporterErrorV1 {
    #[must_use]
    pub const fn is_retryable_startup(&self) -> bool {
        matches!(self, Self::ProjectionStorageUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use outbe_node::ocomp::retention::RetentionError;
    use outbe_ocomp_protocol::opening::OpeningSubjectsV1;

    use crate::rpc_projection::RpcProjectionErrorV1;

    use super::{bisect_opening_subjects, is_lysis_opening_capacity_error, projection_open_error};

    #[test]
    fn only_transient_projection_unavailability_is_retryable_at_startup() {
        let unavailable = projection_open_error(RpcProjectionErrorV1::StorageUnavailable);
        assert!(unavailable.is_retryable_startup());

        let invalid = projection_open_error(RpcProjectionErrorV1::InvalidConfig);
        assert!(!invalid.is_retryable_startup());
    }

    #[test]
    fn oversized_opening_subjects_are_bisected_in_canonical_order() {
        let subjects = OpeningSubjectsV1 {
            owners: (1_u8..=5).map(Address::with_last_byte).collect(),
            reference_isos: vec![840, 978],
        };
        let (left, right) = bisect_opening_subjects(subjects.clone()).expect("splittable");

        assert_eq!(left.owners, subjects.owners[..2]);
        assert_eq!(right.owners, subjects.owners[2..]);
        assert_eq!(left.reference_isos, subjects.reference_isos);
        assert_eq!(right.reference_isos, subjects.reference_isos);
        assert!(left.owners.len() < subjects.owners.len());
        assert!(right.owners.len() < subjects.owners.len());

        let singleton = OpeningSubjectsV1 {
            owners: vec![Address::with_last_byte(1)],
            reference_isos: vec![840],
        };
        assert!(bisect_opening_subjects(singleton).is_none());
    }

    #[test]
    fn only_lysis_opening_byte_capacity_triggers_bisection() {
        assert!(is_lysis_opening_capacity_error(&RetentionError::Source(
            "Lysis opening bytes exceeds cap: 317499 > 262144".to_owned(),
        )));
        assert!(!is_lysis_opening_capacity_error(&RetentionError::Source(
            "raw contract opening bytes exceeds cap: 317499 > 262144".to_owned(),
        )));
        assert!(!is_lysis_opening_capacity_error(
            &RetentionError::RetainedTributeStorageUnavailable,
        ));
    }
}
