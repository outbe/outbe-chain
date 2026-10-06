//! Offline OCOMP observations at the verified current canonical state.

use alloy_primitives::{B256, U256};
use eyre::ensure;
use outbe_intex::schema::SeriesId;
use outbe_primitives::time::WorldwideDay;

use super::Incomplete;
use crate::snapshot::projection_store::projection_database;

use super::canonical_state::CanonicalState;
use outbe_intex::schema::CertifiedContributorGenerationProjection;
use outbe_nod::schema::NodCertifiedGenerationProjection;
use outbe_ocomp::payout_artifact::{
    verify_contributor_payout_artifact, PayoutArtifactError, CONTRIBUTOR_PAYOUT_ARTIFACT_FILE,
};
use outbe_ocomp::{
    admission_catalog::AdmissionCatalogReader,
    bundle::PinnedProtocolBundle,
    cas::{CasLimits, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::poc_input_list_limits,
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    lysis_plan_audit::LocalLysisPlanAuditV1,
    nod_materialization::build_nod_materialization_batch_with_references,
};
use outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1;
use outbe_ocomp_protocol::state::OcompJobRecordV1;
use outbe_snapshot::layout::{validate_layout, ProtectedPaths};
use reth_ethereum::provider::db::{
    cursor::DbCursorRO,
    database::Database,
    mdbx::{create_db, DatabaseArguments},
    table::{Table, TableInfo},
    transaction::{DbTx, DbTxMut},
    DatabaseEnv, TableSet,
};
use std::path::Path;

mod admissions;
pub(crate) use admissions::{
    verify_present_admissions, verify_present_local_results, ExportInputsAudit, NodInputsAudit,
};

mod exports;
use exports::canonical_job_spec;
pub(crate) use exports::{verify_export_inputs, verify_present_discovery};

mod pins;
pub(crate) use pins::{verify_pin_authority, VerifiedPin};

mod inventory;
use inventory::{scan_budget, InventoryRows};
pub(crate) use inventory::{CanonicalInventory, InventoryBounds, ReferenceMembership};

mod bundles;
use bundles::{classify_nod_input_error, missing_native_input, read_pinned_bundle};

mod frames;
pub(crate) use frames::{locate_request_job, verify_closure, verify_retained_frames, ClosureAudit};

mod canonical;
pub(crate) use canonical::verify_canonical_obligations;

mod cas;
pub(crate) use cas::{verify_present_cas, verify_present_receipt};

mod present_join;
use present_join::{
    existing_directory, existing_file, present_count, scan_present_jobs, PresentJobUnion,
    PresentJoinErrors, PRESENT_ACK, PRESENT_ADMISSIONS, PRESENT_BINDING, PRESENT_INPUTS,
    PRESENT_RECEIPT, PRESENT_REFERENCES,
};

mod relations;
pub(crate) use relations::verify_ocomp_relations;
use relations::{
    compare_surviving_ack_export, compare_surviving_result_binding, increment_present,
    validate_present_manifest, PresentArtifactCounts, PresentJobContext,
};

mod present_job;
use present_job::verify_present_job;

mod projection;
use projection::verify_present_projection_structure;

#[cfg(all(test, feature = "snapshot-integration"))]
pub(crate) use admissions::verify_local_result;

pub(crate) use pins::verify_lease_inputs;

pub(crate) use frames::verify_paid_bitmap;

pub(crate) use frames::verify_series_day;

#[cfg(all(test, feature = "snapshot-integration"))]
pub(crate) use canonical::{CanonicalActiveAudit, CanonicalLocalPinStage};
