use crate::NodeHostNoiseKey;

use alloy_primitives::keccak256;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;
use outbe_primitives::tee_attestation_v1::AttestationOperationV1;
use outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1;

use outbe_primitives::tee_attestation_v1::NodeIdV1;
use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use std::fs;

use std::fs::File;
use std::fs::OpenOptions;

use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::fs::PermissionsExt as _;

use std::path::PathBuf;

use super::*;
use ed25519_dalek::Signer as _;
use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
    TransitionKeyReadyProofV1,
};

mod fixtures;
use fixtures::{
    replacement_fixture, replacement_fixture_for_mode, replacement_fixture_for_operation,
    ReplacementFixture,
};

mod committed_join;

mod replacement;

mod locking;
