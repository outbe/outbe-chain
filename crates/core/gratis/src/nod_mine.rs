//! Amount-private NOD mining. Applied and refused answers are both signed.
//!
//! Tag 9 is the mined result. Tag 8 is the signed refusal. Decrypt failures and
//! a non-`In` section stay unsigned transport errors.

use alloy_primitives::{B256, U256};
use outbe_core_common::confidential_tee::{
    attestation, consensus_witness_call, hash, DualWitnessAnswer, FIDELITY, GRATIS, NOD,
};
use outbe_core_common::nod_encryption::EncryptedNodV1;
use outbe_fidelity::{
    apply_cohort_section, EngineError, FidelityCohortOp, FidelityOpOutcome, FidelityOpSection,
};
use outbe_nod::nod_encryption::{self, NodEncryptionError};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::resident::StateKeyHandle;
use outbe_tee::tee_call::{
    verify_enclave_attestation_lazy, verify_signed_rejection, SignedRejectionRef,
};
use outbe_tee::{AppErrorDescriptor, CallMeta, TeeAttestationError, TransportError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::engine::apply_op;
use crate::errors::{display_string, map_consensus_error, nod_mint_rejection};
use crate::protocol::{GratisOp, GratisOpRequest, GratisOpStatus};
use crate::resident::resident_state_key;

const MINE_OP: &str = "MineEncryptedNodV1";
const REJECTED_DOMAIN: &[u8] = b"outbe/nod/mint-rejected/v1";
const STATE_EPOCH: u64 = 0;

/// Attestation descriptor whose context is the historical NOD sentence.
pub(crate) const NOD_MINE_ATTESTATION: AppErrorDescriptor =
    AppErrorDescriptor::new("nod_attestation", "nod attestation signature invalid");

/// Request to mine Gratis from an encrypted NOD record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodRequestV1 {
    /// Encrypted NOD being redeemed.
    pub nod: EncryptedNodV1,
    /// Owner's current encrypted Gratis balance blob.
    pub current_balance: Vec<u8>,
    /// Owner's modify-key MAC authorization.
    pub modify_auth: crate::protocol::ModifyAuth,
    /// Co-located Fidelity cohort acquisition section.
    pub fidelity: FidelityOpSection,
}

/// Result of mining Gratis from an encrypted NOD record. Tag 9.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MineEncryptedNodResultV1 {
    /// Updated encrypted Gratis balance blob.
    pub new_balance: Vec<u8>,
    /// Next expected Gratis op nonce.
    pub next_op_nonce: u64,
    /// Updated Fidelity cohort outcome.
    pub fidelity: FidelityOpOutcome,
    /// Echoed canonical input commitment.
    pub inputs_canonical_hash: B256,
    /// Ed25519 tag. Cleared inside the witness payload.
    pub attestation_tag: Vec<u8>,
    /// Owner-encrypted Gratis mint event amount.
    pub encrypted_event_amount: Vec<u8>,
}

/// Signed refusal of an encrypted NOD mint. Tag 8.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EncryptedNodMintRejectedV1 {
    /// Signed consensus sentence.
    pub reason: String,
    /// Keccak of the canonical mine request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 tag over `outbe/nod/mint-rejected/v1`.
    pub attestation_tag: Vec<u8>,
}

outbe_core_common::impl_confidential_witness_schema!(MineEncryptedNodResultV1, EncryptedNodMinedV1);

outbe_core_common::impl_confidential_witness_schema!(
    EncryptedNodMintRejectedV1,
    EncryptedNodMintRejectedV1
);

/// Resident keys for one mine. The pure engine does not read the host context.
#[derive(Clone, Copy)]
pub struct MineKeys<'a> {
    /// NOD state key that opens the encrypted amount.
    pub nod: &'a StateKeyHandle,
    /// Gratis state key that applies the mint.
    pub gratis: &'a StateKeyHandle,
    /// Fidelity state key that applies the cohort section.
    pub fidelity: &'a StateKeyHandle,
}

/// Failure of the pure mine. Only [`Self::Rejected`] and [`Self::Fidelity`] are signed.
#[derive(Debug, Error)]
pub enum MineError {
    /// Gratis envelope cryptography failed.
    #[error(transparent)]
    Crypto(#[from] outbe_core_common::confidential_tee::TeeError),
    /// The NOD ciphertext did not open.
    #[error(transparent)]
    Nod(#[from] NodEncryptionError),
    /// The cohort section failed. Display is the signed refusal sentence.
    #[error("fidelity section failed: {0}")]
    Fidelity(#[source] EngineError),
    /// Gratis rejected the mint. `reason` is already the consensus sentence.
    #[error("{reason}")]
    Rejected {
        /// Gratis rejection sentence.
        reason: String,
    },
    /// Canonical JSON for the inputs hash did not serialize.
    #[error("NOD mine inputs hash failed")]
    Inputs(#[source] serde_json::Error),
    /// The request chain is not the resident manifest chain.
    #[error("request chain differs from resident chain")]
    ChainMismatch,
    /// The resident chain identifier does not fit in `u64`.
    #[error("resident chain identifier exceeds u64")]
    ChainOverflow,
}

/// Canonical commitment over a [`MineEncryptedNodRequestV1`].
pub fn inputs_hash(
    request: &MineEncryptedNodRequestV1,
) -> std::result::Result<B256, serde_json::Error> {
    hash(b"outbe/nod/mine-inputs/v1", request)
}

/// Domain-separated preimage signed for tag 9. The hash is not inserted again.
pub fn result_preimage(
    result: &MineEncryptedNodResultV1,
) -> std::result::Result<Vec<u8>, serde_json::Error> {
    let mut result = result.clone();
    result.attestation_tag.clear();
    let mut bytes = b"outbe/nod/mine-result/v1".to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(&result)?);
    Ok(bytes)
}

/// Apply the mint and the cohort section. Chain binding is the host's job.
pub fn apply(
    keys: MineKeys<'_>,
    request: &MineEncryptedNodRequestV1,
) -> std::result::Result<MineEncryptedNodResultV1, MineError> {
    if request.fidelity.op != FidelityCohortOp::In {
        return Err(outbe_core_common::confidential_tee::TeeError::DecryptFailed(None).into());
    }
    let amount = nod_encryption::decrypt_nod(keys.nod, &request.nod)?;
    let inputs = inputs_hash(request).map_err(MineError::Inputs)?;
    let operation = mint_request(request, amount);
    let result = apply_op(keys.gratis, &operation);
    if let GratisOpStatus::Rejected { reason } = result.status {
        return Err(MineError::Rejected { reason });
    }
    let fidelity =
        apply_cohort_section(keys.fidelity, operation.account, amount, &request.fidelity)
            .map_err(MineError::Fidelity)?;
    Ok(MineEncryptedNodResultV1 {
        new_balance: result.new_balance,
        encrypted_event_amount: result.encrypted_event_amount,
        next_op_nonce: result.next_op_nonce,
        fidelity,
        inputs_canonical_hash: inputs,
        attestation_tag: Vec::new(),
    })
}

fn mint_request(request: &MineEncryptedNodRequestV1, amount: U256) -> GratisOpRequest {
    GratisOpRequest {
        op: GratisOp::Mint,
        chain_id: B256::from(U256::from(request.nod.terms.chain_id)),
        account: request.nod.terms.owner,
        amount,
        current_balance: request.current_balance.clone(),
        current_pledged: Vec::new(),
        modify_auth: request.modify_auth.clone(),
        fidelity: Some(request.fidelity.clone()),
        sealed_pledge: None,
    }
}

/// Witnessed mine answer before a signed refusal becomes revert bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MineRound {
    /// Tag 9. The attestation tag is on the result.
    Mined(MineEncryptedNodResultV1),
    /// Tag 8. The attestation tag is on the refusal.
    Refused(EncryptedNodMintRejectedV1),
}

/// Mine through the witness tape and keep a signed refusal intact.
///
/// `expected` is computed before the call. This path does not use the
/// in-process stand-in; [`crate::enclave_client::mine_encrypted_nod`] does.
pub fn mine_round(
    request: MineEncryptedNodRequestV1,
) -> outbe_primitives::error::Result<MineRound> {
    let expected = inputs_hash(&request)
        .map_err(|error| map_consensus_error(MINE_OP, TransportError::codec(error)))?;
    let answer = consensus_witness_call(
        CallMeta::recorded(MINE_OP),
        |context| execute(context, expected, &request),
        |key, answer| authenticate(expected, key, answer),
    )
    .map_err(|error| map_consensus_error(MINE_OP, error))?;
    finish(expected, answer)
}

/// Mine through the witness tape. `expected` is computed before the call.
pub(crate) fn witnessed(
    request: MineEncryptedNodRequestV1,
) -> outbe_primitives::error::Result<MineEncryptedNodResultV1> {
    match mine_round(request)? {
        MineRound::Mined(result) => Ok(result),
        MineRound::Refused(rejection) => Err(nod_mint_rejection(rejection.reason)),
    }
}

type MineAnswer = DualWitnessAnswer<MineEncryptedNodResultV1, EncryptedNodMintRejectedV1>;

fn execute(
    context: &InProcessTeeContext,
    expected: B256,
    request: &MineEncryptedNodRequestV1,
) -> std::result::Result<MineAnswer, TransportError> {
    let nod = resident_state_key(context, &NOD, STATE_EPOCH)?;
    let gratis = resident_state_key(context, &GRATIS, STATE_EPOCH)?;
    let fidelity = resident_state_key(context, &FIDELITY, STATE_EPOCH)?;
    require_chain(context, request.nod.terms.chain_id)?;
    let answer = match apply(
        MineKeys {
            nod: &nod,
            gratis: &gratis,
            fidelity: &fidelity,
        },
        request,
    ) {
        Ok(result) => sign_mined(context, result)?,
        Err(error) => match refusal_reason(&error) {
            Some(reason) => sign_refusal(context, expected, &reason)?,
            None => return Err(TransportError::enclave_error(error)),
        },
    };
    authenticate(expected, &context.attestation_public(), &answer)?;
    Ok(answer)
}

fn sign_mined(
    context: &InProcessTeeContext,
    mut result: MineEncryptedNodResultV1,
) -> std::result::Result<MineAnswer, TransportError> {
    let preimage = result_preimage(&result).map_err(TransportError::codec)?;
    result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
    Ok(DualWitnessAnswer::Applied(result))
}

fn sign_refusal(
    context: &InProcessTeeContext,
    inputs_canonical_hash: B256,
    reason: &str,
) -> std::result::Result<MineAnswer, TransportError> {
    let preimage = attestation(REJECTED_DOMAIN, inputs_canonical_hash, reason)
        .map_err(TransportError::codec)?;
    Ok(DualWitnessAnswer::Rejected(EncryptedNodMintRejectedV1 {
        reason: reason.to_owned(),
        inputs_canonical_hash,
        attestation_tag: context.sign_domain_attestation(&preimage).to_vec(),
    }))
}

fn refusal_reason(error: &MineError) -> Option<String> {
    match error {
        MineError::Rejected { reason } => Some(reason.clone()),
        MineError::Fidelity(_) => Some(display_string(error)),
        MineError::Crypto(_)
        | MineError::Nod(_)
        | MineError::Inputs(_)
        | MineError::ChainMismatch
        | MineError::ChainOverflow => None,
    }
}

fn require_chain(
    context: &InProcessTeeContext,
    requested: u64,
) -> std::result::Result<(), TransportError> {
    let chain = U256::from_be_bytes(context.manifest().chain_id);
    let resident = u64::try_from(chain)
        .map_err(|_| TransportError::enclave_error(MineError::ChainOverflow))?;
    if requested != resident {
        return Err(TransportError::enclave_error(MineError::ChainMismatch));
    }
    Ok(())
}

pub(crate) fn authenticate(
    expected: B256,
    key: &[u8; 32],
    answer: &MineAnswer,
) -> std::result::Result<(), TransportError> {
    match answer {
        DualWitnessAnswer::Applied(result) => authenticate_mined(expected, key, result),
        DualWitnessAnswer::Rejected(rejection) => authenticate_refusal(expected, key, rejection),
    }
}

fn authenticate_mined(
    expected: B256,
    key: &[u8; 32],
    result: &MineEncryptedNodResultV1,
) -> std::result::Result<(), TransportError> {
    if result.inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    verify_enclave_attestation_lazy(key, &result.attestation_tag, || result_preimage(result))
        .map_err(|cause| TransportError::app_attestation(NOD_MINE_ATTESTATION, cause))
}

fn authenticate_refusal(
    expected: B256,
    key: &[u8; 32],
    rejection: &EncryptedNodMintRejectedV1,
) -> std::result::Result<(), TransportError> {
    verify_signed_rejection(
        key,
        SignedRejectionRef {
            domain: REJECTED_DOMAIN,
            expected_inputs_hash: expected,
            actual_inputs_hash: rejection.inputs_canonical_hash,
            reason: &rejection.reason,
            tag: &rejection.attestation_tag,
        },
    )
    .map_err(|cause| match cause {
        TeeAttestationError::InputsHashMismatch { .. } => TransportError::UnexpectedResponse,
        other => TransportError::app_attestation(NOD_MINE_ATTESTATION, other),
    })
}

fn finish(expected: B256, answer: MineAnswer) -> outbe_primitives::error::Result<MineRound> {
    match answer {
        DualWitnessAnswer::Applied(result) => {
            if result.inputs_canonical_hash != expected {
                return Err(crate::errors::GratisFatalError::EnclaveNondeterminism.into());
            }
            Ok(MineRound::Mined(result))
        }
        DualWitnessAnswer::Rejected(rejection) => {
            if rejection.inputs_canonical_hash != expected {
                return Err(crate::errors::GratisFatalError::EnclaveNondeterminism.into());
            }
            Ok(MineRound::Refused(rejection))
        }
    }
}

/// Stand-in mapping. Signed refusals revert. Other mine failures revert as fatal
/// stand-in errors, matching the historical test-enclave path.
#[cfg(any(test, feature = "test-enclave"))]
pub(crate) fn map_stand_in(error: MineError) -> outbe_primitives::error::PrecompileError {
    match error {
        MineError::Rejected { reason } => nod_mint_rejection(reason),
        MineError::Fidelity(_) => {
            crate::errors::GratisError::Rejected(display_string(&error)).into()
        }
        other => crate::errors::GratisFatalError::TestEnclave {
            source: Box::new(other),
        }
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolError;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn verified_refusal_keeps_its_reason_and_a_changed_reason_fails_before_use() {
        let signer = SigningKey::from_bytes(&[0x44; 32]);
        let expected = B256::repeat_byte(0x11);
        let reason = "bad \"quote\"\nline";
        let preimage = attestation(REJECTED_DOMAIN, expected, reason).unwrap();
        let tag = signer.sign(&preimage).to_bytes().to_vec();
        let key = signer.verifying_key().to_bytes();
        let rejection = EncryptedNodMintRejectedV1 {
            reason: reason.to_owned(),
            inputs_canonical_hash: expected,
            attestation_tag: tag,
        };
        assert!(authenticate_refusal(expected, &key, &rejection).is_ok());
        let wrong_domain = EncryptedNodMintRejectedV1 {
            reason: reason.to_owned(),
            inputs_canonical_hash: expected,
            attestation_tag: signer
                .sign(
                    &attestation(b"outbe/nod/issue-at-offer-result/v1", expected, reason).unwrap(),
                )
                .to_bytes()
                .to_vec(),
        };
        assert!(matches!(
            authenticate_refusal(expected, &key, &wrong_domain),
            Err(TransportError::AppAttestation { .. })
        ));
        let MineRound::Refused(got) =
            finish(expected, DualWitnessAnswer::Rejected(rejection.clone())).unwrap()
        else {
            panic!("expected a signed refusal");
        };
        assert_eq!(got.reason, reason);
        let err = nod_mint_rejection(got.reason);
        let outbe_primitives::error::PrecompileError::RevertBytes(bytes) = err else {
            panic!("expected revert bytes");
        };
        assert_eq!(
            bytes,
            alloy_primitives::Bytes::from(
                crate::precompile::IGratis::Rejected {
                    value: reason.into(),
                }
                .abi_encode()
            )
        );
        let mut changed = rejection;
        changed.reason = "changed".to_owned();
        assert!(matches!(
            authenticate_refusal(expected, &key, &changed),
            Err(TransportError::AppAttestation { .. })
        ));
        changed.inputs_canonical_hash = B256::ZERO;
        changed.reason = reason.to_owned();
        assert!(matches!(
            authenticate_refusal(expected, &key, &changed),
            Err(TransportError::UnexpectedResponse)
        ));
    }
}
