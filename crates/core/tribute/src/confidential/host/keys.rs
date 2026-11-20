//! Session `outbe_deriveTributeKeys`. No witness entry is written.

use std::fmt::Write as _;

use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::TRIBUTE;
use outbe_primitives::error::PrecompileError;
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::TransportError;
use thiserror::Error;

use outbe_core_common::confidential_tee::{attestation, verify_response_attestation};

use super::call::{self, auth_spec};

const DERIVE_OP: &str = "outbe_deriveTributeKeys";

/// Sealed Tribute view and modify keys for `outbe_deriveTributeKeys`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedTributeKeys {
    /// Account the keys belong to.
    pub account: Address,
    /// Ciphertext sealed to the caller's ephemeral key.
    pub sealed: Vec<u8>,
    /// AEAD nonce.
    pub nonce: [u8; 12],
    /// Enclave X25519 ephemeral public key.
    pub enclave_ephemeral_pubkey: [u8; 32],
}

/// Owner-authorized request for [`derive_tribute_keys`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeriveTributeKeysRequest {
    /// Account whose keys are sealed.
    pub account: Address,
    /// Caller X25519 public key.
    pub requester_ephemeral_pubkey: [u8; 32],
    /// 65-byte owner signature over the Tribute derive-keys message.
    pub owner_sig: Vec<u8>,
}

#[derive(Debug, Error)]
enum DeriveKeysError {
    #[error("owner signature must be 65 bytes")]
    OwnerSignatureLength,
}

/// Seal the account's Tribute keys. This call is not written to the witness tape.
pub fn derive_tribute_keys(
    request: DeriveTributeKeysRequest,
) -> Result<SealedTributeKeys, PrecompileError> {
    let expected = derive_inputs_hash(&request);
    call::session(DERIVE_OP, move |context| {
        execute(context, &request, expected)
    })
    .map_err(map_derive_error)
}

fn execute(
    context: &InProcessTeeContext,
    request: &DeriveTributeKeysRequest,
    expected: B256,
) -> Result<SealedTributeKeys, TransportError> {
    let signature: [u8; 65] = request
        .owner_sig
        .as_slice()
        .try_into()
        .map_err(|_| TransportError::enclave_error(DeriveKeysError::OwnerSignatureLength))?;
    let state_key = call::tribute_state_key(context)?;
    let keys = TRIBUTE
        .account_keys
        .derive_account_keys(&state_key, request.account)
        .map_err(TransportError::enclave_error)?;
    let sealed = TRIBUTE
        .export_sealed_account_keys(&keys, &signature, &request.requester_ephemeral_pubkey)
        .map_err(TransportError::enclave_error)?;
    let answer = SealedTributeKeys {
        account: request.account,
        sealed: sealed.sealed,
        nonce: sealed.nonce,
        enclave_ephemeral_pubkey: sealed.enclave_ephemeral_pubkey,
    };
    let preimage = attestation(
        b"outbe/tribute/derive-keys-result/v1",
        expected,
        &answer.sealed,
    )
    .map_err(call::reject)?;
    let tag = call::sign(context, &preimage);
    authenticate(
        expected,
        &context.attestation_public(),
        expected,
        &tag,
        &preimage,
    )?;
    Ok(answer)
}

fn authenticate(
    expected: B256,
    key: &[u8; 32],
    actual: B256,
    tag: &[u8],
    preimage: &[u8],
) -> Result<(), TransportError> {
    verify_response_attestation(&auth_spec(key, expected, actual, tag), preimage)
}

fn derive_inputs_hash(request: &DeriveTributeKeysRequest) -> B256 {
    let mut bytes = b"outbe/tribute/derive-keys/v1".to_vec();
    bytes.extend_from_slice(request.account.as_slice());
    bytes.extend_from_slice(&request.requester_ephemeral_pubkey);
    bytes.extend_from_slice(&request.owner_sig);
    alloy_primitives::keccak256(bytes)
}

fn map_derive_error(error: TransportError) -> PrecompileError {
    if error.is_node_local() {
        let mut text = String::new();
        let _ = write!(&mut text, "{error}");
        return PrecompileError::EnclaveUnavailable(text);
    }
    PrecompileError::FatalWithCause(Box::new(error))
}
