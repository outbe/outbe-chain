//! V1 DCAP/remote-attestation protocol primitives.
//!
//! This module is always compiled and is on the live consensus path. For
//! example, the block-1 `TeeBootstrap` system tx precharge uses
//! [`SystemGasScheduleV1::normative`]. The node, CLI, and enclave binaries also
//! use these codecs for enclave and registry flows. Changes here must stay
//! deterministic across validators.

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
/// Exact length of an AMD SEV-SNP attestation report.
pub const SEV_SNP_REPORT_BYTES: usize = 1184;
/// Largest DER certificate of the AMD key hierarchy in SEV-SNP evidence.
pub const MAX_SEV_SNP_CERTIFICATE_BYTES: usize = 4 * 1024;
/// Domain of the SEV-SNP launch measurement projection into a rule field.
pub const SEV_SNP_MEASUREMENT_DOMAIN_V1: &[u8] = b"outbe/tee/sev-snp/measurement/v1";
/// `isv_prod_id` of a measurement rule and of a verdict in the SEV-SNP mode.
pub const SEV_SNP_RULE_PRODUCT_ID_V1: u16 = 1;
/// `isv_svn` of a verdict and `minimum_isv_svn` of a rule in the SEV-SNP mode.
pub const SEV_SNP_RULE_SVN_V1: u16 = 1;

/// Project a 48-byte SEV-SNP launch measurement into the 32-byte `mrenclave`
/// field of a measurement rule.
pub fn sev_snp_measurement_rule_value(measurement: &[u8; 48]) -> B256 {
    let mut preimage = Vec::with_capacity(SEV_SNP_MEASUREMENT_DOMAIN_V1.len() + measurement.len());
    preimage.extend_from_slice(SEV_SNP_MEASUREMENT_DOMAIN_V1);
    preimage.extend_from_slice(measurement);
    keccak256(preimage)
}
/// Domain of the TDX image measurement projection into a rule field.
pub const TDX_IMAGE_DOMAIN_V1: &[u8] = b"outbe/tee/tdx/image/v1";
/// Domain of the TDX `MRCONFIGID` projection into a rule field.
pub const TDX_MRCONFIGID_DOMAIN_V1: &[u8] = b"outbe/tee/tdx/mrconfigid/v1";
/// `isv_prod_id` of a measurement rule and of a verdict in the TDX mode.
pub const TDX_RULE_PRODUCT_ID_V1: u16 = 1;
/// `isv_svn` of a verdict and `minimum_isv_svn` of a rule in the TDX mode.
pub const TDX_RULE_SVN_V1: u16 = 1;
/// Quote header version of the TDX policy profile.
pub const TDX_QUOTE_VERSION_V1: u16 = 4;
/// TEE type of a TDX quote header.
pub const TDX_TEE_TYPE_V1: u32 = 0x81;

/// Project the build-time measurement `MRTD` and the runtime registers
/// `RTMR0`, `RTMR1` and `RTMR2` of a TD into the 32-byte `mrenclave` field of
/// a measurement rule. `RTMR3` is not part of the image identity.
pub fn tdx_image_rule_value(mrtd: &[u8; 48], rtmrs: &[[u8; 48]; 3]) -> B256 {
    let mut preimage = Vec::with_capacity(TDX_IMAGE_DOMAIN_V1.len() + 4 * 48);
    preimage.extend_from_slice(TDX_IMAGE_DOMAIN_V1);
    preimage.extend_from_slice(mrtd);
    for rtmr in rtmrs {
        preimage.extend_from_slice(rtmr);
    }
    keccak256(preimage)
}

/// Project the 48-byte `MRCONFIGID` of a TD into the 32-byte `mrsigner` field
/// of a measurement rule.
pub fn tdx_mrconfigid_rule_value(mr_config_id: &[u8; 48]) -> B256 {
    let mut preimage = Vec::with_capacity(TDX_MRCONFIGID_DOMAIN_V1.len() + mr_config_id.len());
    preimage.extend_from_slice(TDX_MRCONFIGID_DOMAIN_V1);
    preimage.extend_from_slice(mr_config_id);
    keccak256(preimage)
}
pub const MAX_COLLATERAL_COMPONENT_BYTES: usize = 896 * 1024;
pub const MAX_ATTESTATION_EVIDENCE_BYTES: usize = 896 * 1024;
pub const MAX_EVIDENCE_CALL_FRAMING_BYTES: usize = 16 * 1024;
pub const MAX_ACTIVE_MEASUREMENT_RULES: usize = 64;
pub const MAX_TEE_POLICY_BYTES: usize = 32 * 1024;
pub const MAX_TEE_POLICY_SCHEDULE_ENTRIES: usize = 64;
pub const MAX_TEE_BOOTSTRAP_BYTES: usize = 1_310_720;
pub use crate::system_tx::STEADY_BLOCK_GAS_LIMIT;

/// No production path reads this constant. It stays `None`, and tests assert
/// that value.
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
    GramineDirectEvidenceV1, SevSnpEvidenceV1,
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
    TeePolicyScheduleV1, TeePolicyV1, INTEL_QE_VENDOR_ID, MAX_TEE_POLICY_SCHEDULE_BYTES,
};
pub use transition::TransitionKeyReadyProofV1;
