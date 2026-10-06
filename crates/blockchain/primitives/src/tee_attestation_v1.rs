//! Inactive V1 DCAP/remote-attestation protocol primitives.
//!
//! This module is compiled only for direct protocol harnesses until the
//! production activation stage. It deliberately defines no selector, storage
//! layout, dispatch route, or active ChainSpec field.

use alloy_primitives::{keccak256, B256};

pub const PROTOCOL_VERSION_V1: u8 = 1;

pub const POLICY_DOMAIN_V1: &[u8] = b"outbe/tee/policy/v1";
pub const POLICY_SCHEDULE_DOMAIN_V1: &[u8] = b"outbe/tee/policy-schedule/v1";
pub const TEE_REGISTRY_GAS_SCHEDULE_DOMAIN_V1: &[u8] = b"outbe/tee-registry-gas-schedule/v1";
pub const SYSTEM_GAS_SCHEDULE_DOMAIN_V1: &[u8] = b"outbe/system-gas-schedule/v1";
pub const RESOURCE_SCHEDULE_DOMAIN_V1: &[u8] = b"outbe/resource-schedule/v1";
pub const NODE_ID_DOMAIN_V1: &[u8] = b"outbe/tee/node-id/v1";
pub const VALIDATOR_NODE_BINDING_DOMAIN_V1: &[u8] = b"outbe/tee/validator-node-binding/v1";
pub const REGISTRATION_INTENT_DOMAIN_V1: &[u8] = b"outbe/tee/registration-intent/v1";
pub const ATTESTATION_EVIDENCE_DOMAIN_V1: &[u8] = b"outbe/tee/attestation-evidence/v1";
pub const ENCLAVE_ID_DOMAIN_V1: &[u8] = b"outbe/tee/enclave-id/v1";
pub const INITIALIZATION_MANIFEST_DOMAIN_V1: &[u8] = b"outbe/tee/initialization-manifest/v1";
pub const NODE_HOST_AUTHORIZATION_DOMAIN_V1: &[u8] = b"outbe/tee/node-host-authorization/v1";
pub const NETWORK_BINDING_DOMAIN_V1: &[u8] = b"outbe/tee/network-binding/v1";
pub const TRUSTED_NETWORK_DESCRIPTOR_DOMAIN_V1: &[u8] = b"outbe/tee/trusted-network-descriptor/v1";
pub const DKG_PARTICIPANT_SET_DOMAIN_V1: &[u8] = b"outbe/tee/dkg-participant-set/v1";
pub const DKG_CEREMONY_DOMAIN_V1: &[u8] = b"outbe/tee/dkg-ceremony/v1";
pub const DKG_PARTICIPANT_ANNOUNCE_DOMAIN_V1: &[u8] = b"outbe/tee/dkg-participant-announce/v1";
pub const TRANSITION_KEY_READY_PROOF_DOMAIN_V1: &[u8] = b"outbe/tee/transition-key-ready-proof/v1";
/// Maximum canonical size of one stable NodeHost authorization witness.
pub const MAX_NODE_HOST_AUTHORIZATION_WITNESS_BYTES: usize = 136;
pub const REPORT_POLICY_DOMAIN_V1: &[u8] = b"outbe/tee/report-policy/v1";

pub const MAX_QUOTE_BYTES: usize = 16 * 1024;
pub const MAX_COLLATERAL_COMPONENT_BYTES: usize = 896 * 1024;
pub const MAX_ATTESTATION_EVIDENCE_BYTES: usize = 896 * 1024;
pub const MAX_EVIDENCE_CALL_FRAMING_BYTES: usize = 16 * 1024;
pub const MAX_ACTIVE_MEASUREMENT_RULES: usize = 64;
pub const MAX_TEE_POLICY_BYTES: usize = 32 * 1024;
pub const MAX_TEE_POLICY_SCHEDULE_ENTRIES: usize = 64;
pub const MAX_TEE_BOOTSTRAP_BYTES: usize = 1_310_720;
pub use crate::system_tx::{BOOTSTRAP_BLOCK_GAS_LIMIT, STEADY_BLOCK_GAS_LIMIT};

/// The stage-I0 feature exposes codecs to direct harnesses only.
///
/// I9 replaces `None` with the ChainSpec-selected production manifest after
/// all preceding acceptance gates pass.
pub const ACTIVE_TEE_ATTESTATION_V1_MANIFEST: Option<TeeAttestationManifestV1> = None;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeeAttestationManifestV1 {
    pub activation_height: u64,
    pub policy_schedule_hash: B256,
    pub resource_schedule_hash: B256,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum CodecError {
    #[error("unexpected end of canonical input")]
    UnexpectedEof,
    #[error("unsupported {field} version {value}")]
    UnsupportedVersion { field: &'static str, value: u8 },
    #[error("unknown {field} discriminant {value:#04x}")]
    UnknownDiscriminant { field: &'static str, value: u8 },
    #[error("{field} length {actual} exceeds limit {limit}")]
    LimitExceeded {
        field: &'static str,
        limit: usize,
        actual: usize,
    },
    #[error("non-canonical {0}")]
    NonCanonical(&'static str),
    #[error("{0} trailing bytes")]
    TrailingBytes(usize),
    #[error("checked arithmetic overflow")]
    ArithmeticOverflow,
    #[error("chain identity mismatch")]
    ChainIdentityMismatch,
}

fn validate_qvl_dimensions(
    evidence_len: usize,
    active_rule_count: usize,
) -> Result<(), CodecError> {
    enforce_limit(
        "attestation evidence",
        MAX_ATTESTATION_EVIDENCE_BYTES,
        evidence_len,
    )?;
    enforce_limit(
        "active measurement rules",
        MAX_ACTIVE_MEASUREMENT_RULES,
        active_rule_count,
    )
}

fn enforce_limit(field: &'static str, limit: usize, actual: usize) -> Result<(), CodecError> {
    if actual > limit {
        return Err(CodecError::LimitExceeded {
            field,
            limit,
            actual,
        });
    }
    Ok(())
}

fn checked_usize(value: usize) -> Result<u64, CodecError> {
    u64::try_from(value).map_err(|_| CodecError::ArithmeticOverflow)
}

fn checked_add_usize(left: usize, right: usize) -> Result<usize, CodecError> {
    left.checked_add(right)
        .ok_or(CodecError::ArithmeticOverflow)
}

fn checked_mul(left: u64, right: u64) -> Result<u64, CodecError> {
    left.checked_mul(right)
        .ok_or(CodecError::ArithmeticOverflow)
}

fn checked_sum(values: &[u64]) -> Result<u64, CodecError> {
    values.iter().try_fold(0u64, |sum, value| {
        sum.checked_add(*value)
            .ok_or(CodecError::ArithmeticOverflow)
    })
}

fn domain_hash(domain: &[u8], canonical: &[u8]) -> B256 {
    let mut preimage = Vec::with_capacity(domain.len() + canonical.len());
    preimage.extend_from_slice(domain);
    preimage.extend_from_slice(canonical);
    keccak256(preimage)
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_len_u32(out: &mut Vec<u8>, value: usize) -> Result<(), CodecError> {
    let value = u32::try_from(value).map_err(|_| CodecError::ArithmeticOverflow)?;
    put_u32(out, value);
    Ok(())
}

struct Decoder<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, position: 0 }
    }

    fn version(&mut self, field: &'static str) -> Result<(), CodecError> {
        let value = self.u8()?;
        if value != PROTOCOL_VERSION_V1 {
            return Err(CodecError::UnsupportedVersion { field, value });
        }
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| CodecError::UnexpectedEof)
    }

    fn declared_len(&mut self, field: &'static str, limit: usize) -> Result<usize, CodecError> {
        let actual = usize::try_from(self.u32()?).map_err(|_| CodecError::ArithmeticOverflow)?;
        enforce_limit(field, limit, actual)?;
        if actual > self.remaining() {
            return Err(CodecError::UnexpectedEof);
        }
        Ok(actual)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(CodecError::ArithmeticOverflow)?;
        let value = self
            .input
            .get(self.position..end)
            .ok_or(CodecError::UnexpectedEof)?;
        self.position = end;
        Ok(value)
    }

    fn remaining(&self) -> usize {
        self.input.len() - self.position
    }

    fn finish(self) -> Result<(), CodecError> {
        if self.remaining() != 0 {
            return Err(CodecError::TrailingBytes(self.remaining()));
        }
        Ok(())
    }
}

mod evidence;
mod gas;
mod identity;
mod intent;
mod network;
mod policy;
mod transition;

pub use evidence::{
    AttestationEvidenceV1, DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
    GramineDirectEvidenceV1,
};
pub use gas::{
    RegistryMutatorV1, ResourceScheduleV1, SystemGasScheduleV1, TeeBootstrapGasInputV1,
    TeeRegistryGasScheduleV1,
};
pub use identity::{
    EnclaveInitializationManifestV1, NodeHostAuthorizationWitnessV1, NodeIdV1,
    ValidatorNodeBindingV1,
};
pub use intent::{AttestationOperationV1, RegistrationIntentV1};
pub use network::{
    dkg_ceremony_id_v1, dkg_participant_announce_hash_v1, dkg_participant_set_hash_v1,
    AttestationMode, NetworkBindingV1, TrustedNetworkDescriptorV1,
};
pub use policy::{
    PlatformTcbStatusSetV1, QvlTcbStatusV1, TeeMeasurementRuleV1, TeePolicyScheduleEntryV1,
    TeePolicyScheduleV1, TeePolicyV1, MAX_TEE_POLICY_SCHEDULE_BYTES,
};
pub use transition::TransitionKeyReadyProofV1;
