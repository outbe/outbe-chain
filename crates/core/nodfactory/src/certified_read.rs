//! Finalized authority projection for proof-backed certified Nod reads.
//!
//! Callers must load both inputs at the same finalized block. Neither Mongo,
//! CAS, nor a proof response may provide the trusted root.

use outbe_nod::NodCertifiedGenerationProjection;
use outbe_ocomp_protocol::{
    result::{ActiveNodSetV1, ExactCountsV1},
    state::ActiveGenerationV1,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CertifiedNodReadError {
    #[error("Metadosis and Nod finalized generation projections do not match")]
    ProjectionMismatch,
}

/// Joins the two independently stored owner projections into the only root
/// authority accepted by a public certified Nod read.
pub fn active_nod_set(
    active: &ActiveGenerationV1,
    nod: &NodCertifiedGenerationProjection,
) -> Result<ActiveNodSetV1, CertifiedNodReadError> {
    if !projections_agree(active, nod) {
        return Err(CertifiedNodReadError::ProjectionMismatch);
    }
    Ok(ActiveNodSetV1 {
        job_id: active.job_id,
        program_semantics_hash: active.program_semantics_hash,
        worldwide_day: nod.worldwide_day.value(),
        generation: nod.generation,
        nod_root: nod.nod_root,
        nod_count: nod.nod_count,
    })
}

fn projections_agree(active: &ActiveGenerationV1, nod: &NodCertifiedGenerationProjection) -> bool {
    active_is_well_formed(active)
        && nod_is_installed(nod)
        && bindings_agree(active, nod)
        && counts_agree(active, nod)
}

fn active_is_well_formed(active: &ActiveGenerationV1) -> bool {
    !active.job_id.is_zero()
        && !active.program_semantics_hash.is_zero()
        && active.availability_certificate_hash.is_none()
}

fn nod_is_installed(nod: &NodCertifiedGenerationProjection) -> bool {
    nod.worldwide_day.value() != 0 && nod.generation != 0 && nod.issued_at != 0
}

fn bindings_agree(active: &ActiveGenerationV1, nod: &NodCertifiedGenerationProjection) -> bool {
    active.job_id == nod.job_id
        && active.program_semantics_hash == nod.program_semantics_hash
        && roots_agree(active, nod)
}

fn roots_agree(active: &ActiveGenerationV1, nod: &NodCertifiedGenerationProjection) -> bool {
    active.nod_root == nod.nod_root
        && active.bucket_root == nod.bucket_root
        && active.output_manifest_root == nod.output_manifest_root
}

fn counts_agree(active: &ActiveGenerationV1, nod: &NodCertifiedGenerationProjection) -> bool {
    let counts = &active.exact_counts;
    counts.tribute_count != 0
        && counts.tribute_count == counts.nod_count
        && counts.tribute_count == nod.tribute_count
        && nod_counts_agree(counts, nod)
}

fn nod_counts_agree(counts: &ExactCountsV1, nod: &NodCertifiedGenerationProjection) -> bool {
    counts.nod_count == nod.nod_count
        && counts.bucket_count == nod.bucket_count
        && nod.bucket_count <= nod.nod_count
}
