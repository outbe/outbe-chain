//! Typed error definitions for the EnclaveRegistry precompile.

use alloy_primitives::{Address, Bytes, B256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

use crate::precompile::IEnclaveRegistry;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum EnclaveRegistryError {
    #[error("caller is not admin")]
    NotAdmin,
    #[error("address must be non-zero")]
    ZeroAddress,
    #[error("already initialized")]
    AlreadyInitialized,
    #[error("input exceeds size cap")]
    InputTooLarge,

    #[error("invalid registration signature")]
    InvalidRegistrationSignature,
    #[error("unsupported registration version {version}")]
    UnsupportedRegistrationVersion { version: u8 },
    #[error("malformed registration payload")]
    MalformedRegistration,
    #[error("attester key does not match recovered signer")]
    AttesterKeyMismatch,
    #[error("caller is not the attester")]
    NotTheAttester,
    #[error("enclave {enclave_pk} did not bind attester {attester}")]
    EnclaveDidNotBindTheAttester { enclave_pk: B256, attester: Address },
    #[error("attester is not active")]
    AttesterNotActive,
    #[error("attester lacks SubmitSpendingUnit permission")]
    AttesterNotPermitted,

    #[error("enclave {enclave_pk} is already registered")]
    AlreadyRegistered { enclave_pk: B256 },
    #[error("enclave {enclave_pk} is not registered")]
    NotRegistered { enclave_pk: B256 },
    #[error("enclave {enclave_pk} is not live")]
    DeploymentNotLive { enclave_pk: B256 },

    #[error("SEV-SNP report refused")]
    ReportRefused,
    #[error("report_data does not match enclave identity binding")]
    IdentityBindingMismatch,
    #[error("guest policy allows debug")]
    DebugPolicy,
    #[error("guest policy allows migration")]
    MigrationPolicy,
    #[error("chip {chip_id_hash} is revoked")]
    ChipRevoked { chip_id_hash: B256 },
    #[error("report was not requested at VMPL0")]
    NotVmpl0,
    #[error("reported TCB {reported:#x} is below floor {floor:#x}")]
    TcbBelowFloor { reported: u64, floor: u64 },
    #[error("report product {reported} does not match recorded product {recorded}")]
    ProductMismatch { reported: u8, recorded: u8 },

    #[error("measurement {measurement_hash} is not allowlisted")]
    MeasurementNotAllowed { measurement_hash: B256 },
    #[error("measurement {measurement_hash} is not certified by registration")]
    MeasurementNotCertified { measurement_hash: B256 },
    #[error("measurement {measurement_hash} is already recorded")]
    AlreadyKnownMeasurement { measurement_hash: B256 },
    #[error("measurement {measurement_hash} is unknown for this deployment")]
    UnknownMeasurement { measurement_hash: B256 },
    #[error("deployment already has maximum number of entries")]
    TooManyEntries,

    #[error("manifest SHA-256 does not match rmDigest")]
    ManifestMismatch,
    #[error("malformed runtime manifest")]
    MalformedManifest,
    #[error("unsupported manifest schema {schema}")]
    UnsupportedManifestSchema { schema: u16 },
    #[error("provider {provider_id} is not allowed")]
    ProviderNotAllowed { provider_id: B256 },
    #[error("manifest effective_base_url does not match provider policy")]
    BaseUrlMismatch,
    #[error("manifest provider_environment does not match provider policy")]
    EnvironmentNotAllowed,
    #[error("provider requires a release build profile")]
    NotReleaseBuild,

    #[error("devnet adminRegister is disabled")]
    DevnetRegistrationDisabled,
}

impl From<EnclaveRegistryError> for PrecompileError {
    fn from(error: EnclaveRegistryError) -> Self {
        use EnclaveRegistryError::*;
        let encoded = match error {
            NotAdmin => IEnclaveRegistry::NotAdmin {}.abi_encode(),
            ZeroAddress => IEnclaveRegistry::ZeroAddress {}.abi_encode(),
            AlreadyInitialized => IEnclaveRegistry::AlreadyInitialized {}.abi_encode(),
            InputTooLarge => IEnclaveRegistry::InputTooLarge {}.abi_encode(),
            InvalidRegistrationSignature => {
                IEnclaveRegistry::InvalidRegistrationSignature {}.abi_encode()
            }
            UnsupportedRegistrationVersion { version } => {
                IEnclaveRegistry::UnsupportedRegistrationVersion { version }.abi_encode()
            }
            MalformedRegistration => IEnclaveRegistry::MalformedRegistration {}.abi_encode(),
            AttesterKeyMismatch => IEnclaveRegistry::AttesterKeyMismatch {}.abi_encode(),
            NotTheAttester => IEnclaveRegistry::NotTheAttester {}.abi_encode(),
            EnclaveDidNotBindTheAttester {
                enclave_pk,
                attester,
            } => IEnclaveRegistry::EnclaveDidNotBindTheAttester {
                enclavePk: enclave_pk,
                attester,
            }
            .abi_encode(),
            AttesterNotActive => IEnclaveRegistry::AttesterNotActive {}.abi_encode(),
            AttesterNotPermitted => IEnclaveRegistry::AttesterNotPermitted {}.abi_encode(),
            AlreadyRegistered { enclave_pk } => IEnclaveRegistry::AlreadyRegistered {
                enclavePk: enclave_pk,
            }
            .abi_encode(),
            NotRegistered { enclave_pk } => IEnclaveRegistry::NotRegistered {
                enclavePk: enclave_pk,
            }
            .abi_encode(),
            DeploymentNotLive { enclave_pk } => IEnclaveRegistry::DeploymentNotLive {
                enclavePk: enclave_pk,
            }
            .abi_encode(),
            ReportRefused => IEnclaveRegistry::ReportRefused {}.abi_encode(),
            IdentityBindingMismatch => IEnclaveRegistry::IdentityBindingMismatch {}.abi_encode(),
            DebugPolicy => IEnclaveRegistry::DebugPolicy {}.abi_encode(),
            MigrationPolicy => IEnclaveRegistry::MigrationPolicy {}.abi_encode(),
            ChipRevoked { chip_id_hash } => IEnclaveRegistry::ChipRevoked {
                chipIdHash: chip_id_hash,
            }
            .abi_encode(),
            NotVmpl0 => IEnclaveRegistry::NotVmpl0 {}.abi_encode(),
            TcbBelowFloor { reported, floor } => {
                IEnclaveRegistry::TcbBelowFloor { reported, floor }.abi_encode()
            }
            ProductMismatch { reported, recorded } => {
                IEnclaveRegistry::ProductMismatch { reported, recorded }.abi_encode()
            }
            MeasurementNotAllowed { measurement_hash } => IEnclaveRegistry::MeasurementNotAllowed {
                measurementHash: measurement_hash,
            }
            .abi_encode(),
            MeasurementNotCertified { measurement_hash } => {
                IEnclaveRegistry::MeasurementNotCertified {
                    measurementHash: measurement_hash,
                }
                .abi_encode()
            }
            AlreadyKnownMeasurement { measurement_hash } => {
                IEnclaveRegistry::AlreadyKnownMeasurement {
                    measurementHash: measurement_hash,
                }
                .abi_encode()
            }
            UnknownMeasurement { measurement_hash } => IEnclaveRegistry::UnknownMeasurement {
                measurementHash: measurement_hash,
            }
            .abi_encode(),
            TooManyEntries => IEnclaveRegistry::TooManyEntries {}.abi_encode(),
            ManifestMismatch => IEnclaveRegistry::ManifestMismatch {}.abi_encode(),
            MalformedManifest => IEnclaveRegistry::MalformedManifest {}.abi_encode(),
            UnsupportedManifestSchema { schema } => {
                IEnclaveRegistry::UnsupportedManifestSchema { schema }.abi_encode()
            }
            ProviderNotAllowed { provider_id } => IEnclaveRegistry::ProviderNotAllowed {
                providerId: provider_id,
            }
            .abi_encode(),
            BaseUrlMismatch => IEnclaveRegistry::BaseUrlMismatch {}.abi_encode(),
            EnvironmentNotAllowed => IEnclaveRegistry::EnvironmentNotAllowed {}.abi_encode(),
            NotReleaseBuild => IEnclaveRegistry::NotReleaseBuild {}.abi_encode(),
            DevnetRegistrationDisabled => {
                IEnclaveRegistry::DevnetRegistrationDisabled {}.abi_encode()
            }
        };
        PrecompileError::RevertBytes(Bytes::from(encoded))
    }
}
