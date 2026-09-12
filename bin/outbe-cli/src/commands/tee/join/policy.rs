use alloy_primitives::Address;
use alloy_primitives::B256;

use eyre::Result;

use outbe_operator::tee::RenewalBindingV1;

use outbe_primitives::tee_attestation_v1::AttestationMode;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use outbe_tee::protocol::EnclaveResponse;

use outbe_tee::CommittedJoinSubmissionV1;

use outbe_tee::ReplacementCandidateSubmissionV1;
use outbe_tee::TransportError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum JoinTransport {
    AuthorizedNodeHost,
    Development,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum JoinOfferKeyState {
    Keyless,
    ReadyExact,
    ReadyMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) struct JoinCompletionPlan {
    pub(in super::super) ingest_offer_key: bool,
    pub(in super::super) promote_candidate: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in super::super) enum MissingCommittedRelayPlan {
    ConstructAndPersist,
    CleanupReadyExact,
}

pub(in super::super) struct ExactJoinRelayV1 {
    pub(in super::super) transaction_hash: B256,
    pub(in super::super) raw_transaction: Vec<u8>,
}

pub(in super::super) enum DurableJoinSubmissionV1 {
    Candidate(ReplacementCandidateSubmissionV1),
    Committed(CommittedJoinSubmissionV1),
}

impl DurableJoinSubmissionV1 {
    pub(in super::super) fn evidence(&self) -> &[u8] {
        match self {
            Self::Candidate(value) => value.evidence(),
            Self::Committed(value) => value.evidence(),
        }
    }

    pub(in super::super) const fn node_signature(&self) -> &[u8; 65] {
        match self {
            Self::Candidate(value) => value.node_signature(),
            Self::Committed(value) => value.node_signature(),
        }
    }

    pub(in super::super) const fn enclave_signature(&self) -> &[u8; 64] {
        match self {
            Self::Candidate(value) => value.enclave_signature(),
            Self::Committed(value) => value.enclave_signature(),
        }
    }

    pub(in super::super) const fn is_candidate(&self) -> bool {
        matches!(self, Self::Candidate(_))
    }

    pub(in super::super) const fn is_committed(&self) -> bool {
        matches!(self, Self::Committed(_))
    }

    pub(in super::super) const fn registration_caller(&self) -> Option<Address> {
        match self {
            Self::Candidate(_) => None,
            Self::Committed(value) => Some(value.registration_caller()),
        }
    }
}

pub(in super::super) fn ensure_durable_join_registration_caller(
    durable_caller: Option<Address>,
    current_caller: Address,
) -> Result<()> {
    if durable_caller.is_some_and(|caller| caller != current_caller) {
        eyre::bail!("durable tee join was created with a different global --private-key");
    }
    Ok(())
}

pub(in super::super) fn plan_missing_committed_relay(
    resumes_finalized_target: bool,
    offer_key_state: JoinOfferKeyState,
) -> Result<MissingCommittedRelayPlan> {
    match (resumes_finalized_target, offer_key_state) {
        (_, JoinOfferKeyState::ReadyMismatch) => {
            eyre::bail!("resident permanent offer key does not match finalized TeeRegistry");
        }
        (false, _) => Ok(MissingCommittedRelayPlan::ConstructAndPersist),
        (true, JoinOfferKeyState::ReadyExact) => Ok(MissingCommittedRelayPlan::CleanupReadyExact),
        (true, JoinOfferKeyState::Keyless) => {
            eyre::bail!(
                "finalized committed binding has no durable pre-relay transaction checkpoint"
            );
        }
    }
}

pub(in super::super) fn plan_join_completion(
    offer_key_state: JoinOfferKeyState,
    is_candidate: bool,
) -> Result<JoinCompletionPlan> {
    if offer_key_state == JoinOfferKeyState::ReadyMismatch {
        eyre::bail!("resident permanent offer key does not match finalized TeeRegistry");
    }
    Ok(JoinCompletionPlan {
        ingest_offer_key: offer_key_state == JoinOfferKeyState::Keyless,
        promote_candidate: is_candidate,
    })
}

pub(in super::super) fn classify_join_offer_key_state(
    response: EnclaveResponse,
    expected_offer_pub: [u8; 32],
) -> Result<JoinOfferKeyState> {
    classify_join_offer_key_state_transport(response, expected_offer_pub)
        .map_err(|error| eyre::eyre!(error))
}

pub(in super::super) fn classify_join_offer_key_state_transport(
    response: EnclaveResponse,
    expected_offer_pub: [u8; 32],
) -> std::result::Result<JoinOfferKeyState, TransportError> {
    match response {
        EnclaveResponse::PublicKeys {
            offer_key_ready: false,
            ..
        } => Ok(JoinOfferKeyState::Keyless),
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            recipient_x25519_pub,
            ..
        } if recipient_x25519_pub == expected_offer_pub => Ok(JoinOfferKeyState::ReadyExact),
        EnclaveResponse::PublicKeys {
            offer_key_ready: true,
            ..
        } => Ok(JoinOfferKeyState::ReadyMismatch),
        other => Err(TransportError::DcapVerification(format!(
            "expected enclave PublicKeys, got {other:?}"
        ))),
    }
}

pub(in super::super) fn select_join_transport(
    attestation_mode: AttestationMode,
    has_node_data_dir: bool,
) -> Result<JoinTransport> {
    match (attestation_mode, has_node_data_dir) {
        (AttestationMode::DcapRequired, true) | (AttestationMode::GramineDirectDev, true) => {
            Ok(JoinTransport::AuthorizedNodeHost)
        }
        (AttestationMode::DcapRequired, false) => Err(eyre::eyre!(
            "DcapRequired tee join requires --node-data-dir"
        )),
        (AttestationMode::GramineDirectDev, false) => Ok(JoinTransport::Development),
    }
}

pub(in super::super) fn ensure_joinable_binding(
    binding: Option<&outbe_operator::tee::RenewalBindingV1>,
    finalized_timestamp: u64,
) -> Result<()> {
    if binding.is_some_and(|binding| finalized_timestamp < binding.valid_until) {
        eyre::bail!("finalized enclave lease is live; use `outbe-cli tee renew`");
    }
    Ok(())
}

pub(in super::super) fn registration_counters(
    binding: Option<&outbe_operator::tee::RenewalBindingV1>,
) -> Result<(u64, u64, u64, u64)> {
    let Some(binding) = binding else {
        return Ok((1, 0, 0, 0));
    };
    Ok((
        binding
            .binding_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("binding version exhausted"))?,
        binding
            .registration_version
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("registration version exhausted"))?,
        binding.renewal_nonce,
        binding.transition_nonce,
    ))
}

pub(in super::super) fn finalized_binding_matches_intent(
    binding: &RenewalBindingV1,
    intent: &RegistrationIntentV1,
) -> Result<bool> {
    Ok(binding.node_id_hash
        == intent
            .node_id
            .node_id_hash()
            .map_err(|error| eyre::eyre!("hash registration node identity: {error}"))?
        && binding.enclave_id == intent.enclave_id
        && binding.binding_id == intent.binding_id
        && binding.intent_hash
            == intent
                .intent_hash()
                .map_err(|error| eyre::eyre!("hash registration intent: {error}"))?
        && binding.binding_version == intent.binding_version
        && binding.registration_version == intent.registration_version
        && binding.renewal_nonce == intent.renewal_nonce
        && binding.transition_nonce == intent.transition_nonce
        && binding.valid_until == intent.requested_valid_until
        && binding.recipient_x25519 == B256::from(intent.recipient_x25519)
        && binding.attestation_ed25519 == B256::from(intent.attestation_ed25519)
        && binding.noise_responder_x25519 == B256::from(intent.noise_responder_x25519)
        && binding.node_host_authorization_hash == intent.node_host_authorization_hash)
}

pub(in super::super) fn committed_manifest_matches_binding(
    manifest: &outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
    binding: &RenewalBindingV1,
) -> Result<bool> {
    Ok(manifest
        .node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("hash committed node identity: {error}"))?
        == binding.node_id_hash
        && manifest
            .enclave_id()
            .map_err(|error| eyre::eyre!("derive committed enclave identity: {error}"))?
            == binding.enclave_id
        && B256::from(manifest.recipient_x25519) == binding.recipient_x25519
        && B256::from(manifest.attestation_ed25519) == binding.attestation_ed25519
        && B256::from(manifest.noise_responder_x25519) == binding.noise_responder_x25519
        && manifest
            .node_host_authorization_hash()
            .map_err(|error| eyre::eyre!("derive committed NodeHost authorization: {error}"))?
            == binding.node_host_authorization_hash)
}
