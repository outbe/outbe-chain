//! TeeRegistry V1 node-enclave registration state machine.
//!
//! A0 activates this schema through the production precompile route. Accepted
//! hardware-free tests still enter only through the private typed post-verifier
//! capability and cannot replace enclave-resident production verification.

use alloy_primitives::{Address, B256, U256};

mod binding;
#[cfg(feature = "tee-attestation-v1")]
mod evidence;
#[cfg(feature = "tee-attestation-v1")]
mod mutation;
mod policy;
#[cfg(feature = "tee-attestation-v1")]
mod registration;
#[cfg(all(test, feature = "tee-attestation-v1"))]
mod test_support;

use outbe_primitives::{
    error::{PrecompileError, Result},
    tee_attestation_v1::{NodeIdV1, TeePolicyV1, MAX_TEE_POLICY_BYTES},
    tee_genesis_v1::is_attestation_mode_allowed_for_chain_id,
};
use outbe_validatorset::contract::ValidatorSet;
#[cfg(feature = "tee-attestation-v1")]
use outbe_validatorset::runtime::status as validator_status;

use crate::schema::TeeRegistry;
#[cfg(feature = "tee-attestation-v1")]
use alloy_primitives::keccak256;

#[cfg(feature = "tee-attestation-v1")]
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, PlatformTcbStatusSetV1,
    RegistrationIntentV1, ValidatorNodeBindingV1,
};
#[cfg(feature = "tee-attestation-v1")]
use outbe_tee::dcap_protocol::{
    dcap_evidence_hash_v1, DcapOnboardingArtifactV1, DcapOnboardingContextV1,
    DcapPlatformTcbStatusV1, DcapVerdictV1, DcapVerificationOutcomeV1,
};

pub use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1::{
    EnclaveBindingReplacedV1, EnclaveMeasurementTransitionedV1, EnclaveRegisteredV1,
    EnclaveRenewedV1, OfferKeySealedForRegistryV1, TeePolicyActivatedV1, ValidatorNodeHostBoundV1,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V1RegistrationOutcome {
    Created,
    Idempotent,
}

#[cfg(feature = "tee-attestation-v1")]
pub(crate) struct V1OnboardingOutcome {
    pub(crate) registration: V1RegistrationOutcome,
    pub(crate) artifact: Option<DcapOnboardingArtifactV1>,
}

#[cfg(feature = "tee-attestation-v1")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VerifiedEnclaveClaimsV1 {
    mrenclave: B256,
    mrsigner: B256,
    isv_prod_id: u16,
    isv_svn: u16,
    collateral_valid_until: u64,
    platform_tcb_status: u8,
    verdict_hash: B256,
}

#[cfg(feature = "tee-attestation-v1")]
struct VerifiedClaimsMutationV1<'a> {
    expected_operation: AttestationOperationV1,
    caller: Option<Address>,
    intent: &'a RegistrationIntentV1,
    node_signature: &'a [u8; 65],
    enclave_signature: &'a [u8; 64],
    policy: &'a TeePolicyV1,
    claims: &'a VerifiedEnclaveClaimsV1,
    evidence_hash: B256,
}

#[cfg(feature = "tee-attestation-v1")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RegistrationCallerContextV1 {
    expired_rejoin: bool,
}

#[cfg(feature = "tee-attestation-v1")]
impl VerifiedEnclaveClaimsV1 {
    fn from_dcap(verdict: &DcapVerdictV1) -> Result<Self> {
        let verdict_bytes = verdict.encode_canonical().map_err(|code| {
            PrecompileError::Fatal(format!(
                "verified DCAP verdict cannot be encoded: {:#06x}",
                code.code()
            ))
        })?;
        Ok(Self {
            mrenclave: verdict.mrenclave,
            mrsigner: verdict.mrsigner,
            isv_prod_id: verdict.isv_prod_id,
            isv_svn: verdict.isv_svn,
            collateral_valid_until: verdict.collateral_valid_until,
            platform_tcb_status: verdict.platform_tcb_status as u8,
            verdict_hash: keccak256(verdict_bytes),
        })
    }
}

#[cfg(feature = "tee-attestation-v1")]
#[derive(Clone, Copy)]
pub struct EnclaveEvidenceV1<'a> {
    pub caller: Address,
    pub evidence: &'a [u8],
    pub node_signature: &'a [u8; 65],
    pub enclave_signature: &'a [u8; 64],
}

#[cfg(feature = "tee-attestation-v1")]
#[derive(Clone, Copy)]
pub struct NodeHostAssociationV1<'a> {
    pub binding: &'a ValidatorNodeBindingV1,
    pub validator_signature: &'a [u8; 65],
    pub node_binding_signature: &'a [u8; 65],
}

#[cfg(all(test, feature = "tee-attestation-v1"))]
pub(crate) struct VerifiedIntentV1<'a> {
    pub(crate) intent: &'a RegistrationIntentV1,
    pub(crate) node_signature: &'a [u8; 65],
    pub(crate) enclave_signature: &'a [u8; 64],
    pub(crate) capability: PostVerifierDcapCapabilityV1,
}

outbe_primitives::define_tee_registry_binding_v1! {
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeEnclaveBindingV1
}

#[cfg(feature = "tee-attestation-v1")]
fn ensure_continuous_binding(
    current: &NodeEnclaveBindingV1,
    intent: &RegistrationIntentV1,
    claims: &VerifiedEnclaveClaimsV1,
) -> Result<()> {
    let same_identity = intent.enclave_id == current.enclave_id
        && intent.binding_id == current.binding_id
        && B256::from(intent.recipient_x25519) == current.recipient_x25519;
    let same_authorization = B256::from(intent.attestation_ed25519) == current.attestation_ed25519
        && B256::from(intent.noise_responder_x25519) == current.noise_responder_x25519
        && B256::from(intent.node_host_authorization_hash) == current.node_host_authorization_hash;
    if !same_identity || !same_authorization {
        return Err(PrecompileError::Revert(
            "renewal targets a superseded or different enclave identity".into(),
        ));
    }
    if !matches_current_measurement(current, claims) {
        return Err(PrecompileError::Revert(
            "renewal cannot replace the admitted enclave measurement".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "tee-attestation-v1")]
fn matches_current_measurement(
    current: &NodeEnclaveBindingV1,
    claims: &VerifiedEnclaveClaimsV1,
) -> bool {
    claims.mrenclave == current.mrenclave
        && claims.mrsigner == current.mrsigner
        && claims.isv_prod_id == current.isv_prod_id
        && claims.isv_svn == current.isv_svn
}

#[cfg(feature = "tee-attestation-v1")]
fn direct_dev_claims(
    policy: &TeePolicyV1,
    height: u64,
    evidence_hash: B256,
) -> Result<VerifiedEnclaveClaimsV1> {
    let mut matching = policy.measurement_rules.iter().filter(|rule| {
        height >= rule.admit_from_height && height < rule.admit_until_height_exclusive
    });
    let rule = matching.next().ok_or_else(|| {
        PrecompileError::Revert(
            "GramineDirectDev policy has no active measurement projection".into(),
        )
    })?;
    if matching.next().is_some() {
        return Err(PrecompileError::Revert(
            "GramineDirectDev policy has overlapping measurement projections".into(),
        ));
    }
    Ok(VerifiedEnclaveClaimsV1 {
        mrenclave: rule.mrenclave,
        mrsigner: rule.mrsigner,
        isv_prod_id: rule.isv_prod_id,
        isv_svn: rule.minimum_isv_svn,
        collateral_valid_until: u64::MAX,
        platform_tcb_status: 0,
        verdict_hash: evidence_hash,
    })
}

#[cfg(feature = "tee-attestation-v1")]
fn next_counter(current: u64, name: &'static str) -> Result<u64> {
    current
        .checked_add(1)
        .ok_or_else(|| PrecompileError::Revert(format!("{name} is exhausted")))
}

#[cfg(feature = "tee-attestation-v1")]
fn ensure_live_binding(current: &NodeEnclaveBindingV1, now: u64) -> Result<()> {
    if now >= current.valid_until {
        return Err(PrecompileError::Revert(
            "enclave lease expired; registerEnclave rejoin is required".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "tee-attestation-v1")]
fn ensure_renewal_window(
    current: &NodeEnclaveBindingV1,
    policy: &TeePolicyV1,
    now: u64,
) -> Result<()> {
    ensure_live_binding(current, now)?;
    if policy.maximum_lease == 0 || !policy.maximum_lease.is_multiple_of(2) {
        return Err(PrecompileError::Fatal(
            "active V1 lease period is not a positive even duration".into(),
        ));
    }
    let opens_at = current.valid_until.saturating_sub(policy.maximum_lease / 2);
    if now < opens_at {
        return Err(PrecompileError::Revert(
            "renewal window has not opened".into(),
        ));
    }
    Ok(())
}

/// Test-only typed capability that begins strictly after public QVL verification.
/// It is absent from every non-test artifact and cannot parse or bless evidence.
#[cfg(all(test, feature = "tee-attestation-v1"))]
pub(crate) struct PostVerifierDcapCapabilityV1 {
    verdict: DcapVerdictV1,
    evidence_hash: B256,
}

#[cfg(all(test, feature = "tee-attestation-v1"))]
impl PostVerifierDcapCapabilityV1 {
    pub(crate) fn new(verdict: DcapVerdictV1) -> Self {
        Self {
            verdict,
            evidence_hash: B256::repeat_byte(0xEC),
        }
    }

    pub(crate) fn with_evidence_hash(verdict: DcapVerdictV1, evidence_hash: B256) -> Self {
        Self {
            verdict,
            evidence_hash,
        }
    }
}

fn chain_id_word(chain_id: u64) -> [u8; 32] {
    U256::from(chain_id).to_be_bytes()
}

fn consensus_timestamp(storage: &outbe_primitives::storage::StorageHandle<'_>) -> Result<u64> {
    u64::try_from(storage.timestamp()?)
        .map_err(|_| PrecompileError::Revert("consensus timestamp exceeds u64".into()))
}

fn checked_u16(value: u64, field: &'static str) -> Result<u16> {
    u16::try_from(value)
        .map_err(|_| PrecompileError::Fatal(format!("{field} exceeds its canonical width")))
}

fn checked_u8(value: u64, field: &'static str) -> Result<u8> {
    u8::try_from(value)
        .map_err(|_| PrecompileError::Fatal(format!("{field} exceeds its canonical width")))
}

fn revert_codec(context: &'static str, error: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::Revert(format!("{context}: {error}"))
}
