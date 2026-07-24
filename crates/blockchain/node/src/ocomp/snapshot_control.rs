//! Exact, one-job snapshot-export authority behind the node-local control seam.
//!
//! The module deliberately owns the coordination between the durable OCOMP pin
//! and the CE next-apply lease. Callers receive bounded protocol values only:
//! no Reth provider, Mongo writer, CE path, or writer-capable tree handle
//! crosses this seam.

use std::sync::{Arc, Mutex, MutexGuard};

use alloy_primitives::B256;
use outbe_compressed_entities::{CompressedTreeService, ExportLeaseStatus, TreeServiceError};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{
        CheckProjectionContainmentV1, CommitSnapshotExportV1, FinalizedIntentProofResponseV1,
        GetSnapshotHandoffV1, ListSnapshotHandoffsResponseV1, ListSnapshotHandoffsV1,
        ProjectionCheckpointV1, ProjectionContainedV1, RenewSnapshotLeaseV1,
        SnapshotExportCommittedV1, SnapshotHandoffV1, SnapshotLeaseOpenedV1,
    },
    input::CheckpointIdentityV1,
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    ProtocolError, SchemaLimits,
};
use outbe_primitives::projection::ProjectionCheckpoint;
use reth_storage_api::{BlockHashReader, BlockIdReader};
use thiserror::Error;

use crate::projection::{ocomp_projection_contains, OcompProjectionContainment};

use super::retention::{OcompRetentionCoordinator, PinStateV1, RetentionError, RetentionStatus};

/// Node-owned canonical-history authority for one Mongo projection checkpoint.
pub trait ProjectionContainmentAuthority: Send + Sync {
    fn require_contains(
        &self,
        checkpoint: ProjectionCheckpoint,
        required: ProjectionCheckpoint,
    ) -> Result<(), SnapshotExportError>;
}

/// Production Reth adapter. Both the projected head and the retained job block
/// must resolve in the node's finalized canonical history.
pub struct RethProjectionContainmentAuthority<P> {
    canonical: P,
}

impl<P> RethProjectionContainmentAuthority<P> {
    #[must_use]
    pub const fn new(canonical: P) -> Self {
        Self { canonical }
    }
}

impl<P> ProjectionContainmentAuthority for RethProjectionContainmentAuthority<P>
where
    P: BlockHashReader + BlockIdReader + Send + Sync,
{
    fn require_contains(
        &self,
        checkpoint: ProjectionCheckpoint,
        required: ProjectionCheckpoint,
    ) -> Result<(), SnapshotExportError> {
        match ocomp_projection_contains(checkpoint, required, &self.canonical)
            .map_err(|error| SnapshotExportError::ProjectionContainment(error.to_string()))?
        {
            OcompProjectionContainment::Contains { .. } => Ok(()),
            OcompProjectionContainment::Behind { .. } => {
                Err(SnapshotExportError::ProjectionBehind {
                    checkpoint_height: checkpoint.block_number,
                    required_height: required.block_number,
                })
            }
        }
    }
}

/// Node-owned one-entry broker for the PoC's one live finalized job.
pub struct SnapshotExportAuthority {
    retention: Arc<OcompRetentionCoordinator>,
    tree: Arc<CompressedTreeService>,
    projection_containment: Arc<dyn ProjectionContainmentAuthority>,
    limits: SchemaLimits,
    handoff: Mutex<Option<SnapshotHandoffV1>>,
}

impl SnapshotExportAuthority {
    #[must_use]
    pub fn new(
        retention: Arc<OcompRetentionCoordinator>,
        tree: Arc<CompressedTreeService>,
        projection_containment: Arc<dyn ProjectionContainmentAuthority>,
        limits: SchemaLimits,
    ) -> Self {
        Self {
            retention,
            tree,
            projection_containment,
            limits,
            handoff: Mutex::new(None),
        }
    }

    /// Arms the exact current finalized CE marker for the exact durable job.
    ///
    /// Repeating the request returns the same live generation. A timed-out
    /// generation may be replaced, but a different job never can.
    pub fn open(&self, job_id: B256) -> Result<SnapshotHandoffV1, SnapshotExportError> {
        let mut slot = self.lock_handoff()?;
        if let Some(existing) = slot.as_ref() {
            if existing.job_id != job_id {
                return Err(SnapshotExportError::ConflictingJob);
            }
            match self.tree.export_lease_status(existing.lease_generation)? {
                ExportLeaseStatus::Pending | ExportLeaseStatus::Opened => {
                    return Ok(existing.clone());
                }
                ExportLeaseStatus::TimedOut => {}
            }
        }

        let (pin_generation, candidate) = match self.retention.status() {
            RetentionStatus::Ready(record) => match record.state {
                PinStateV1::Finalized {
                    candidate,
                    job_id: live_job,
                } if live_job == job_id => (record.generation, candidate),
                _ => return Err(SnapshotExportError::JobNotFinalized),
            },
            RetentionStatus::Empty => return Err(SnapshotExportError::JobNotFinalized),
            RetentionStatus::Quarantined { reason } => {
                return Err(SnapshotExportError::RetentionQuarantined(reason));
            }
        };
        let marker = self.tree.finalized_marker()?;
        if marker.height != candidate.block_number || marker.block_hash != candidate.block_hash {
            return Err(SnapshotExportError::CeMarkerMismatch {
                expected_height: candidate.block_number,
                expected_hash: candidate.block_hash,
                actual_height: marker.height,
                actual_hash: marker.block_hash,
            });
        }
        let ce_schema_version = u16::try_from(
            self.tree
                .environment_identity()
                .local_storage_schema_version,
        )
        .map_err(|_| SnapshotExportError::CeSchemaVersionOverflow)?;
        let offer = self.tree.arm_finalized_export(marker)?;
        let handoff = SnapshotHandoffV1 {
            job_id,
            pin_generation,
            lease_generation: offer.generation(),
            checkpoint: CheckpointIdentityV1 {
                finalized_block_number: candidate.block_number,
                finalized_block_hash: candidate.block_hash,
                finalized_state_root: candidate.state_root,
                finalized_ce_root: marker.new_root,
                ce_schema_version,
            },
            canonical_lease_offer: BoundedBytes(offer.encode_fixed().to_vec()),
        };
        let _ = handoff.encode_body(&self.limits)?;
        *slot = Some(handoff.clone());
        Ok(handoff)
    }

    pub fn list(
        &self,
        request: &ListSnapshotHandoffsV1,
    ) -> Result<ListSnapshotHandoffsResponseV1, SnapshotExportError> {
        let _ = request.encode_body(&self.limits)?;
        let slot = self.lock_handoff()?;
        let Some(handoff) = slot
            .as_ref()
            .filter(|handoff| handoff.lease_generation > request.after_lease_generation)
        else {
            return Ok(ListSnapshotHandoffsResponseV1 {
                next_lease_generation: request.after_lease_generation,
                handoffs: Vec::new(),
            });
        };
        Ok(ListSnapshotHandoffsResponseV1 {
            next_lease_generation: handoff.lease_generation,
            handoffs: vec![handoff.clone()],
        })
    }

    pub fn get(
        &self,
        request: &GetSnapshotHandoffV1,
    ) -> Result<SnapshotHandoffV1, SnapshotExportError> {
        let _ = request.encode_body(&self.limits)?;
        self.lock_handoff()?
            .as_ref()
            .filter(|handoff| {
                handoff.job_id == request.job_id
                    && handoff.lease_generation == request.lease_generation
            })
            .cloned()
            .ok_or(SnapshotExportError::HandoffNotFound)
    }

    pub fn acknowledge_open(
        &self,
        request: &RenewSnapshotLeaseV1,
    ) -> Result<SnapshotLeaseOpenedV1, SnapshotExportError> {
        let _ = request.encode_body(&self.limits)?;
        let handoff = self.get(&GetSnapshotHandoffV1 {
            job_id: request.job_id,
            lease_generation: request.lease_generation,
        })?;
        self.tree
            .acknowledge_export_open_bytes(&request.canonical_open_ack.0)?;
        if self.tree.export_lease_status(handoff.lease_generation)? != ExportLeaseStatus::Opened {
            return Err(SnapshotExportError::LeaseNotOpened);
        }
        Ok(SnapshotLeaseOpenedV1 {
            job_id: handoff.job_id,
            lease_generation: handoff.lease_generation,
        })
    }

    pub fn build_finalized_intent_proof(
        &self,
        job_id: B256,
    ) -> Result<FinalizedIntentProofResponseV1, SnapshotExportError> {
        let proof = self.retention.build_finalized_intent_proof(job_id)?;
        Ok(FinalizedIntentProofResponseV1 {
            job_id,
            canonical_proof: BoundedBytes(proof.encode_canonical(&self.limits)?),
        })
    }

    pub fn build_lysis_openings(
        &self,
        job_id: B256,
        subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, SnapshotExportError> {
        self.retention
            .build_lysis_openings(job_id, subjects)
            .map_err(Into::into)
    }

    pub fn check_projection_containment(
        &self,
        request: &CheckProjectionContainmentV1,
    ) -> Result<ProjectionContainedV1, SnapshotExportError> {
        let handoff = self.get(&GetSnapshotHandoffV1 {
            job_id: request.job_id,
            lease_generation: request.lease_generation,
        })?;
        let projection_checkpoint = ProjectionCheckpoint {
            block_number: request.projection_checkpoint.block_number,
            block_hash: request.projection_checkpoint.block_hash,
        };
        let required_checkpoint = ProjectionCheckpoint {
            block_number: handoff.checkpoint.finalized_block_number,
            block_hash: handoff.checkpoint.finalized_block_hash,
        };
        self.projection_containment
            .require_contains(projection_checkpoint, required_checkpoint)?;
        Ok(ProjectionContainedV1 {
            job_id: handoff.job_id,
            lease_generation: handoff.lease_generation,
            projection_checkpoint: request.projection_checkpoint.clone(),
            required_checkpoint: ProjectionCheckpointV1 {
                block_number: required_checkpoint.block_number,
                block_hash: required_checkpoint.block_hash,
            },
        })
    }

    pub fn commit(
        &self,
        request: &CommitSnapshotExportV1,
    ) -> Result<SnapshotExportCommittedV1, SnapshotExportError> {
        let _ = request.encode_body(&self.limits)?;
        let handoff = self.get(&GetSnapshotHandoffV1 {
            job_id: request.job_id,
            lease_generation: request.lease_generation,
        })?;
        if handoff.pin_generation != request.pin_generation {
            return Err(SnapshotExportError::PinGenerationMismatch {
                expected: handoff.pin_generation,
                actual: request.pin_generation,
            });
        }
        if self.tree.export_lease_status(handoff.lease_generation)? != ExportLeaseStatus::Opened {
            return Err(SnapshotExportError::LeaseNotOpened);
        }
        let durable = self.retention.record_exported(
            request.job_id,
            request.pin_generation,
            request.lease_generation,
            request.manifest_hash,
        )?;
        Ok(SnapshotExportCommittedV1 {
            job_id: request.job_id,
            pin_generation: durable.generation,
            record_hash: durable.record_hash,
        })
    }

    fn lock_handoff(
        &self,
    ) -> Result<MutexGuard<'_, Option<SnapshotHandoffV1>>, SnapshotExportError> {
        self.handoff
            .lock()
            .map_err(|_| SnapshotExportError::Poisoned)
    }
}

#[derive(Debug, Error)]
pub enum SnapshotExportError {
    #[error(transparent)]
    Tree(Box<TreeServiceError>),
    #[error(transparent)]
    Retention(#[from] RetentionError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("snapshot export authority mutex is poisoned")]
    Poisoned,
    #[error("snapshot export handoff already belongs to a different job")]
    ConflictingJob,
    #[error("requested OCOMP job is not in the exact finalized pin state")]
    JobNotFinalized,
    #[error("OCOMP retention is quarantined: {0}")]
    RetentionQuarantined(String),
    #[error(
        "CE marker mismatch: expected {expected_height}/{expected_hash}, got {actual_height}/{actual_hash}"
    )]
    CeMarkerMismatch {
        expected_height: u64,
        expected_hash: B256,
        actual_height: u64,
        actual_hash: B256,
    },
    #[error("CE local storage schema version does not fit the protocol field")]
    CeSchemaVersionOverflow,
    #[error("snapshot handoff was not found")]
    HandoffNotFound,
    #[error("snapshot lease did not reach opened state")]
    LeaseNotOpened,
    #[error("snapshot pin generation mismatch: expected {expected}, got {actual}")]
    PinGenerationMismatch { expected: u64, actual: u64 },
    #[error(
        "Mongo projection is behind the finalized job: checkpoint {checkpoint_height}, required {required_height}"
    )]
    ProjectionBehind {
        checkpoint_height: u64,
        required_height: u64,
    },
    #[error("Mongo projection containment failed: {0}")]
    ProjectionContainment(String),
}

impl From<TreeServiceError> for SnapshotExportError {
    fn from(error: TreeServiceError) -> Self {
        Self::Tree(Box::new(error))
    }
}
