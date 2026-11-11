//! Runtime-specific boundary artifact fixtures.

use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381::{
    self, dkg::feldman_desmedt::Output, primitives::variant::MinSig,
};
use outbe_primitives::{consensus::DkgBoundaryArtifact, validators::ValidatorSet};

pub use outbe_simplex::test_harness::fixtures::*;

pub struct BoundaryFixtureSettings {
    pub is_full_dkg: bool,
    pub freeze_height: u64,
    pub planned_activation_height: u64,
    pub is_validator_set_change: bool,
}

pub fn boundary_artifact(
    epoch: u64,
    validator_set: &ValidatorSet,
    output: &Output<MinSig, bls12381::PublicKey>,
    settings: BoundaryFixtureSettings,
) -> DkgBoundaryArtifact {
    crate::dkg_manager::build_boundary_artifact(crate::dkg_manager::BoundaryArtifactInput {
        epoch: Epoch::new(epoch),
        validator_set,
        output,
        is_full_dkg: settings.is_full_dkg,
        dkg_cycle: epoch,
        freeze_height: settings.freeze_height,
        planned_activation_height: settings.planned_activation_height,
        vrf_material_version: epoch,
        is_validator_set_change: settings.is_validator_set_change,
        tee_expired_target_exclusions: Vec::new(),
    })
    .unwrap()
}
