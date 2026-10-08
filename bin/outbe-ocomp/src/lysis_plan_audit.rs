//! Incremental cold-restart audit for one plan-bound Lysis V1 artifact set.
//!
//! Every cursor step processes at most one bounded catalog entry, input chunk,
//! unit artifact or directory entry. This layer does not bind finalized job
//! authority and does not validate phase payload semantics. Therefore neither
//! an individual item nor `Complete` is a signing/finalization capability.

use std::collections::BTreeSet;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::derive_poseidon_entity_id;
use outbe_lysis::program_v1::planner::{
    LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1, PlannedProducerV1,
    PlannedUnitPositionV1, PlannerErrorV1,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::RunUnitV1,
    input::{
        AuthenticatedOpeningV1, InputChunkKind, InputChunkRefV1, InputManifestV1, OpeningSourceKind,
    },
    list::try_streaming_ordered_list_membership_proof,
    unit::{PlanCommitmentV1, UnitArtifactV1, UnitPhase, UnitSpecV1},
    CasObjectRefV1, ListKind, ObjectKind, ProtocolError, SchemaLimits, StreamingOrderedListRoot,
};
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

use crate::{
    admission_catalog::{
        AdmissionCatalogError, AdmissionCatalogReader, AdmissionDirectoryCursorV1,
        AdmissionDirectoryStepV1, VerifiedAdmissionCatalog, VerifiedAdmissionRecordV1,
    },
    bundle::PinnedProtocolBundle,
    cas::{CasError, FilesystemCasReader},
    input_artifacts::{decode_fidelity_subject_key, decode_oracle_subject_key, InputArtifactError},
    input_ref_catalog::{
        InputRefCatalogClosureCursorV1, InputRefCatalogClosureStepV1, InputRefCatalogError,
        VerifiedInputChunkRefCatalog, VerifiedInputChunkRefV1,
    },
};

mod cursor;
mod schedule;

const MAX_SETTLEMENT_ISOS: usize = 256;

pub struct LocalLysisPlanAuditV1<'a> {
    schedule: VerifiedPlanSchedule<'a>,
}

struct VerifiedPlanSchedule<'a> {
    admissions: &'a VerifiedAdmissionCatalog,
    input_refs: &'a VerifiedInputChunkRefCatalog,
    reader: &'a FilesystemCasReader,
    bundle: &'a PinnedProtocolBundle,
    limits: &'a SchemaLimits,
    plan_ref: CasObjectRefV1,
    manifest_ref: CasObjectRefV1,
    manifest: InputManifestV1,
    plan: PlanCommitmentV1,
    planner: LysisPlannerV1,
    topology: LysisPlanTopologyV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanBoundLysisArtifactV1 {
    membership: PlannedArtifactMembership,
    execution: AdmittedArtifactExecution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedArtifactMembership {
    plan_ordinal: u32,
    position: PlannedUnitPositionV1,
    spec: UnitSpecV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AdmittedArtifactExecution {
    artifact: UnitArtifactV1,
    admission: VerifiedAdmissionRecordV1,
}

impl PlanBoundLysisArtifactV1 {
    #[must_use]
    pub const fn plan_ordinal(&self) -> u32 {
        self.membership.plan_ordinal
    }

    #[must_use]
    pub const fn position(&self) -> PlannedUnitPositionV1 {
        self.membership.position
    }

    #[must_use]
    pub const fn spec(&self) -> &UnitSpecV1 {
        &self.membership.spec
    }

    #[must_use]
    pub const fn artifact(&self) -> &UnitArtifactV1 {
        &self.execution.artifact
    }

    #[must_use]
    pub const fn admission(&self) -> &VerifiedAdmissionRecordV1 {
        &self.execution.admission
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LysisPlanAuditStepV1 {
    InputChecked { ordinal: u32, kind: InputChunkKind },
    FidelityOwnerMembershipProbe { owner: Address },
    FidelityOwnerMembershipChecked { owner: Address },
    InputReferenceListClosed,
    InputCatalogEntryChecked,
    InputsClosed,
    Artifact(Box<PlanBoundLysisArtifactV1>),
    ArtifactsClosed,
    AdmissionCatalogEntryChecked,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LysisPlanAuditStageV1 {
    InputCatalog,
    Artifacts,
    AdmissionCatalog,
    Complete,
}

struct OwnerTributeSearchV1 {
    owner: Address,
    tribute_id: Vec<u8>,
    low: u32,
    high: u32,
}

pub struct LysisPlanAuditCursorV1<'a> {
    audit: &'a LocalLysisPlanAuditV1<'a>,
    stage: LysisPlanAuditStageV1,
    input_catalog: Option<InputRefCatalogClosureCursorV1<'a>>,
    admission_catalog: Option<AdmissionDirectoryCursorV1<'a>>,
    primary_root: Option<StreamingOrderedListRoot>,
    pending_tribute_ref: Option<InputChunkRefV1>,
    primary_spec_count: u32,
    fidelity_opening_count: u32,
    oracle_opening_count: u32,
    fidelity_root: Option<StreamingOrderedListRoot>,
    oracle_root: Option<StreamingOrderedListRoot>,
    tribute_count: u32,
    tribute_nominal_total: U256,
    tribute_isos: BTreeSet<u16>,
    previous_tribute_last_key: Option<Vec<u8>>,
    previous_fidelity_owner: Option<Address>,
    pending_fidelity_owners: Vec<Address>,
    next_fidelity_owner: usize,
    owner_tribute_search: Option<OwnerTributeSearchV1>,
    fidelity_owner_count: u32,
    oracle_subject_isos: Option<Vec<u16>>,
    next_artifact_ordinal: u32,
    failed: bool,
}

/// Runs the native plan audit against an immutable admission reader.
pub fn open_read_only_local_plan_audit<'a>(
    admissions: &'a AdmissionCatalogReader,
    input_refs: &'a VerifiedInputChunkRefCatalog,
    reader: &'a FilesystemCasReader,
    bundle: &'a PinnedProtocolBundle,
    limits: &'a SchemaLimits,
) -> Result<LocalLysisPlanAuditV1<'a>, ExactLysisPlanError> {
    open_local_plan_audit(
        admissions.verified_view(),
        input_refs,
        reader,
        bundle,
        limits,
    )
}

pub fn open_local_plan_audit<'a>(
    admissions: &'a VerifiedAdmissionCatalog,
    input_refs: &'a VerifiedInputChunkRefCatalog,
    reader: &'a FilesystemCasReader,
    bundle: &'a PinnedProtocolBundle,
    limits: &'a SchemaLimits,
) -> Result<LocalLysisPlanAuditV1<'a>, ExactLysisPlanError> {
    let pinned = admissions.reload_pinned_plan(reader)?;
    input_refs.require_manifest_authority(&pinned.input_manifest_ref, &pinned.input_manifest)?;
    if bundle.hash() != pinned.plan.protocol_bundle_hash
        || pinned.input_manifest.protocol_bundle_hash != bundle.hash()
    {
        return Err(ExactLysisPlanError::AuthorityMismatch(
            "protocol bundle hash",
        ));
    }
    pinned
        .input_manifest
        .validate_against_bundle(bundle.bundle(), limits)?;
    if pinned.plan.planner_spec_version != bundle.bundle().planner_spec_version
        || pinned.plan.reducer_spec_version != bundle.bundle().reducer_spec_version
    {
        return Err(ExactLysisPlanError::AuthorityMismatch(
            "planner and reducer versions",
        ));
    }

    let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
        protocol_bundle_hash: pinned.plan.protocol_bundle_hash,
        job_id: pinned.plan.job_id,
        attempt: pinned.plan.attempt,
        input_manifest_hash: pinned.plan.input_manifest_hash,
        input_manifest_encoded_bytes: pinned.input_manifest_ref.encoded_bytes,
        fidelity_opening_root: pinned.input_manifest.fidelity_opening_root,
        oracle_opening_root: pinned.input_manifest.oracle_opening_root,
        wwd: pinned.plan.wwd,
        lysis_limit_minor: pinned.plan.lysis_limit_minor,
        logical_evaluation_time: pinned.plan.logical_evaluation_time,
        tribute_count: pinned.plan.tribute_count,
        lysis_program_semantics_hash: bundle.bundle().lysis_program_semantics_hash,
        planner_spec_version: bundle.bundle().planner_spec_version,
        reducer_spec_version: bundle.bundle().reducer_spec_version,
    })?;
    let topology = LysisPlanTopologyV1::new(pinned.plan.primary_work_unit_count)?;

    Ok(LocalLysisPlanAuditV1 {
        schedule: VerifiedPlanSchedule {
            admissions,
            input_refs,
            reader,
            bundle,
            limits,
            plan_ref: pinned.plan_ref,
            manifest_ref: pinned.input_manifest_ref,
            manifest: pinned.input_manifest,
            plan: pinned.plan,
            planner,
            topology,
        },
    })
}

impl<'a> LocalLysisPlanAuditV1<'a> {
    /// Produces a scheduler candidate. It is not a finalization input.
    pub fn candidate_spec_at(&self, plan_ordinal: u32) -> Result<UnitSpecV1, ExactLysisPlanError> {
        self.schedule.derive_spec_at(plan_ordinal)
    }

    /// Prepares the exact bounded worker request for one ready plan member.
    ///
    /// Producer references come only from durable verified admissions. Input
    /// references come only from the closed manifest-bound input catalog.
    /// Enumerate membership is generated with a bounded streaming frontier.
    pub fn worker_request_at(&self, plan_ordinal: u32) -> Result<RunUnitV1, ExactLysisPlanError> {
        self.schedule.worker_request_at(plan_ordinal)
    }

    pub fn audit_cursor(&'a self) -> Result<LysisPlanAuditCursorV1<'a>, ExactLysisPlanError> {
        let fidelity_opening_count = self
            .schedule
            .manifest
            .exact_record_count
            .checked_sub(self.schedule.manifest.tribute_count)
            .and_then(|count| count.checked_sub(1))
            .ok_or(ExactLysisPlanError::AuthorityMismatch(
                "manifest opening record counts",
            ))?;
        if fidelity_opening_count == 0 {
            return Err(ExactLysisPlanError::AuthorityMismatch(
                "manifest Fidelity opening count",
            ));
        }
        Ok(LysisPlanAuditCursorV1 {
            audit: self,
            stage: LysisPlanAuditStageV1::InputCatalog,
            input_catalog: Some(self.schedule.input_refs.bounded_closure_cursor()?),
            admission_catalog: None,
            primary_root: Some(StreamingOrderedListRoot::new(
                ListKind::UnitSpecificationsArtifacts,
                self.schedule.plan.primary_work_unit_count,
            )?),
            pending_tribute_ref: None,
            primary_spec_count: 0,
            fidelity_opening_count: 0,
            oracle_opening_count: 0,
            fidelity_root: Some(StreamingOrderedListRoot::new(
                ListKind::FidelityOpenings,
                fidelity_opening_count,
            )?),
            oracle_root: Some(StreamingOrderedListRoot::new(ListKind::OracleOpenings, 1)?),
            tribute_count: 0,
            tribute_nominal_total: U256::ZERO,
            tribute_isos: BTreeSet::from([840]),
            previous_tribute_last_key: None,
            previous_fidelity_owner: None,
            pending_fidelity_owners: Vec::new(),
            next_fidelity_owner: 0,
            owner_tribute_search: None,
            fidelity_owner_count: 0,
            oracle_subject_isos: None,
            next_artifact_ordinal: 0,
            failed: false,
        })
    }

    #[must_use]
    pub const fn plan(&self) -> &PlanCommitmentV1 {
        &self.schedule.plan
    }

    #[must_use]
    pub const fn manifest(&self) -> &InputManifestV1 {
        &self.schedule.manifest
    }

    #[must_use]
    pub(crate) const fn bundle(&self) -> &PinnedProtocolBundle {
        self.schedule.bundle
    }

    pub(crate) fn verified_artifact_at(
        &self,
        plan_ordinal: u32,
    ) -> Result<PlanBoundLysisArtifactV1, ExactLysisPlanError> {
        let admission = self.plan_bound_admission_at(plan_ordinal)?;
        let position = self.schedule.topology.plan_position_at(plan_ordinal)?;
        let spec = self.schedule.derive_spec_at(plan_ordinal)?;
        if admission.unit_id != spec.unit_id(self.schedule.limits)? {
            return Err(ExactLysisPlanError::UnexpectedUnitId { plan_ordinal });
        }
        let object = self
            .schedule
            .reader
            .read_verified(&admission.artifact_ref)?;
        let artifact = UnitArtifactV1::decode_canonical(object.bytes(), self.schedule.limits)?;
        artifact.validate_against(&spec, self.schedule.limits)?;
        Ok(PlanBoundLysisArtifactV1 {
            membership: PlannedArtifactMembership {
                plan_ordinal,
                position,
                spec,
            },
            execution: AdmittedArtifactExecution {
                artifact,
                admission,
            },
        })
    }

    pub(crate) fn plan_bound_admission_at(
        &self,
        plan_ordinal: u32,
    ) -> Result<VerifiedAdmissionRecordV1, ExactLysisPlanError> {
        let admission = self.schedule.admissions.read(plan_ordinal)?;
        self.schedule.require_admission_authority(&admission)?;
        Ok(admission)
    }

    pub(crate) const fn reader(&self) -> &FilesystemCasReader {
        self.schedule.reader
    }

    pub(crate) const fn admissions(&self) -> &VerifiedAdmissionCatalog {
        self.schedule.admissions
    }

    pub(crate) const fn limits(&self) -> &SchemaLimits {
        self.schedule.limits
    }

    pub(crate) fn bounded_admission_directory_cursor(
        &self,
    ) -> Result<AdmissionDirectoryCursorV1<'_>, ExactLysisPlanError> {
        self.schedule
            .admissions
            .bounded_directory_cursor()
            .map_err(Into::into)
    }
}

fn input_object_ref(reference: &InputChunkRefV1) -> CasObjectRefV1 {
    CasObjectRefV1 {
        transport_digest: reference.transport_digest,
        encoded_bytes: reference.encoded_bytes,
        expected_ocb1_kind: Some(ObjectKind::AuthenticatedInputChunkV1.tag()),
    }
}

fn required_unit_id(
    producer_ids: &[Option<B256>],
    index: usize,
) -> Result<B256, ExactLysisPlanError> {
    producer_ids
        .get(index)
        .copied()
        .flatten()
        .filter(|unit_id| !unit_id.is_zero())
        .ok_or(ExactLysisPlanError::AuthorityMismatch(
            "required producer UnitId",
        ))
}

fn exact_pair(producer_ids: &[Option<B256>]) -> Result<[Option<B256>; 2], ExactLysisPlanError> {
    match producer_ids {
        [left, right] => Ok([*left, *right]),
        _ => Err(ExactLysisPlanError::AuthorityMismatch(
            "binary producer count",
        )),
    }
}

#[derive(Debug, Error)]
pub enum ExactLysisPlanError {
    #[error("private Tribute amount read failed: {0}")]
    PrivateTribute(#[from] outbe_tee::TransportError),
    #[error(transparent)]
    Admission(#[from] AdmissionCatalogError),
    #[error(transparent)]
    InputRef(#[from] InputRefCatalogError),
    #[error(transparent)]
    InputArtifact(#[from] InputArtifactError),
    #[error(transparent)]
    TributeBody(#[from] outbe_compressed_entities::CanonicalBodyError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error(transparent)]
    Planner(#[from] PlannerErrorV1),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("Lysis plan authority mismatch: {0}")]
    AuthorityMismatch(&'static str),
    #[error("plan ordinal {plan_ordinal} admitted an unexpected UnitId")]
    UnexpectedUnitId { plan_ordinal: u32 },
}
