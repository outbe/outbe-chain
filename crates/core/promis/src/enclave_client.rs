//! Host caller for confidential Promis writes and account-key delivery.
//!
//! `ApplyPromisOp` is witness-recorded. Its inputs hash is fixed before the
//! call and checked again inside `execute` against the same context that signed. A foreign chain is a signed modify-MAC
//! rejection, not a host chain check. `outbe_derivePromisKeys` is session-only.

use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::{
    consensus_witness_call, seal_resident_account_keys, session_witness_call,
    verify_bound_witness_attestation, PROMIS,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::{CallMeta, TransportError};
use thiserror::Error;

use crate::attest::verify_promis_op_attestation;
use crate::engine::apply_op;
use crate::errors::{map_consensus_error, PromisFatalError};
use crate::protocol::{
    promis_op_attestation_preimage, promis_op_canonical_hash, PromisOpRequest, PromisOpResult,
};
use crate::resident::resident_state_key;

const APPLY_OP: &str = "ApplyPromisOp";
const DERIVE_OP: &str = "outbe_derivePromisKeys";
const STATE_EPOCH: u64 = 0;

/// Sealed Promis view and modify keys for `outbe_derivePromisKeys`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedPromisKeys {
    /// Account the keys belong to.
    pub account: Address,
    /// Ciphertext sealed to the caller's ephemeral key.
    pub sealed: Vec<u8>,
    /// AEAD nonce.
    pub nonce: [u8; 12],
    /// Enclave X25519 ephemeral public key.
    pub enclave_ephemeral_pubkey: [u8; 32],
}

/// Owner-authorized request for [`derive_promis_keys`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivePromisKeysRequest {
    /// Account whose keys are sealed.
    pub account: Address,
    /// Caller X25519 public key.
    pub requester_ephemeral_pubkey: [u8; 32],
    /// 65-byte owner signature over the Promis derive-keys message.
    pub owner_sig: Vec<u8>,
}

#[derive(Debug, Error)]
enum DeriveKeysError {
    #[error("owner signature must be 65 bytes")]
    OwnerSignatureLength,
}

/// Run one Promis op. A business rejection stays inside the returned result.
pub fn apply_promis_op(req: PromisOpRequest) -> Result<PromisOpResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_apply(&req) {
        return Ok(result);
    }
    let expected = promis_op_canonical_hash(&req);
    let result = consensus_witness_call(
        CallMeta::recorded(APPLY_OP),
        |context| execute_promis(context, expected, &req),
        |key, result| authenticate_promis(expected, key, result),
    )
    .map_err(|error| map_consensus_error(APPLY_OP, error))?;
    finish_promis(expected, result)
}

/// Seal the account's Promis keys. This call is not written to the witness tape.
pub fn derive_promis_keys(request: DerivePromisKeysRequest) -> Result<SealedPromisKeys> {
    let sealed = session_witness_call(CallMeta::session_only(DERIVE_OP), |context| {
        execute_derive(context, &request)
    })
    .ok_or(PromisFatalError::SidecarUnavailable { context: DERIVE_OP })?
    .map_err(map_derive_error)?;
    Ok(sealed)
}

/// An owner-proof rejection stays a request failure. A resident outage stays unavailable.
fn map_derive_error(error: TransportError) -> PrecompileError {
    if error.is_node_local() {
        return map_consensus_error(DERIVE_OP, error).into();
    }
    PrecompileError::FatalWithCause(Box::new(map_consensus_error(DERIVE_OP, error)))
}

fn execute_promis(
    context: &InProcessTeeContext,
    expected: B256,
    req: &PromisOpRequest,
) -> std::result::Result<PromisOpResult, TransportError> {
    let key = resident_state_key(context, &PROMIS, STATE_EPOCH)?;
    let mut result = apply_op(&key, req);
    let preimage = promis_op_attestation_preimage(result.inputs_canonical_hash, &result)
        .map_err(TransportError::codec)?;
    result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
    authenticate_promis(expected, &context.attestation_public(), &result)?;
    Ok(result)
}

fn execute_derive(
    context: &InProcessTeeContext,
    request: &DerivePromisKeysRequest,
) -> std::result::Result<SealedPromisKeys, TransportError> {
    seal_resident_account_keys(
        context,
        (&PROMIS, STATE_EPOCH),
        (
            request.account,
            request.owner_sig.as_slice(),
            &request.requester_ephemeral_pubkey,
        ),
        || DeriveKeysError::OwnerSignatureLength,
        |account, sealed| SealedPromisKeys {
            account,
            sealed: sealed.sealed,
            nonce: sealed.nonce,
            enclave_ephemeral_pubkey: sealed.enclave_ephemeral_pubkey,
        },
    )
}

fn authenticate_promis(
    expected: B256,
    key: &[u8; 32],
    result: &PromisOpResult,
) -> std::result::Result<(), TransportError> {
    verify_bound_witness_attestation(expected, key, result, verify_promis_op_attestation)
}

fn finish_promis(expected: B256, result: PromisOpResult) -> Result<PromisOpResult> {
    if result.inputs_canonical_hash != expected {
        return Err(PromisFatalError::EnclaveNondeterminism.into());
    }
    Ok(result)
}

/// In-process stand-in. It runs the real engine and skips attestation.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use super::*;
    use crate::engine::{
        derive_account_keys, derive_promis_state_key, modify_mac, ModifyOperation,
    };
    use crate::protocol::{ModifyAuth, PromisOp};
    use alloy_primitives::U256;
    use std::cell::RefCell;

    thread_local! {
        static STATE_KEY: RefCell<Option<outbe_tee::resident::StateKeyHandle>> = const { RefCell::new(None) };
    }

    const DEV_EPOCH: u64 = 0;
    const PROMIS_GROUP_SIG: &[u8] = b"outbe-dev-promis-group-signature-fixed-seed!!";
    const PROMIS_CHAIN: B256 = B256::repeat_byte(0xC1);
    fn offer(group: &[u8], chain: B256) -> outbe_tee::resident::ResidentOfferMaterial {
        outbe_tee::resident::ResidentOfferMaterial::for_test(group.to_vec(), chain, DEV_EPOCH, 0)
            .expect("dev offer material")
    }

    fn fixture_key() -> outbe_tee::resident::StateKeyHandle {
        let material = offer(PROMIS_GROUP_SIG, PROMIS_CHAIN);
        derive_promis_state_key(&material, PROMIS_CHAIN, DEV_EPOCH)
            .expect("derive dev promis state key")
    }

    /// Install the stand-in for this thread.
    pub fn install() {
        STATE_KEY.with(|key| *key.borrow_mut() = Some(fixture_key()));
    }

    /// Remove the stand-in for this thread.
    pub fn uninstall() {
        STATE_KEY.with(|key| *key.borrow_mut() = None);
    }

    /// Dev state handle, so a test can build a MAC and open a balance.
    pub fn state_key() -> outbe_tee::resident::StateKeyHandle {
        assert!(
            STATE_KEY.with(|key| key.borrow().is_some()),
            "test enclave not installed"
        );
        fixture_key()
    }

    /// Authorization bound to chain 1 and the installed fixture key.
    pub fn modify_auth(op: PromisOp, account: Address, amount: U256, nonce: u64) -> ModifyAuth {
        let key = derive_account_keys(&state_key(), account).expect("account keys");
        ModifyAuth {
            mac: modify_mac(
                &key,
                &ModifyOperation {
                    account,
                    op,
                    amount,
                    op_nonce: nonce,
                    chain_id: B256::from(U256::from(1u64)),
                },
            ),
            op_nonce: nonce,
        }
    }

    pub(crate) fn try_apply(req: &PromisOpRequest) -> Option<PromisOpResult> {
        STATE_KEY.with(|slot| slot.borrow().as_ref().map(|key| apply_op(key, req)))
    }
}

#[cfg(test)]
mod executing_context {
    use super::*;
    use crate::engine::{derive_account_keys, modify_mac, ModifyOperation};
    use crate::protocol::{ModifyAuth, PromisOp};
    use alloy_primitives::{Address, U256};
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
    use outbe_tee::resident::ResidentOfferMaterial;
    use std::sync::Arc;

    fn binding() -> NetworkBindingV1 {
        NetworkBindingV1 {
            chain_id: U256::from(1u64).to_be_bytes(),
            genesis_hash: B256::repeat_byte(2),
            attestation_mode: AttestationMode::NoAttestationDev,
        }
    }

    fn chain() -> B256 {
        B256::from(binding().chain_id)
    }

    fn context(seed: u8) -> Arc<InProcessTeeContext> {
        let context = Arc::new(outbe_tee::test_utils::resident_context(seed, binding()));
        let material = ResidentOfferMaterial::for_test(
            b"promis-witness-group-signature-v1!!!!!".to_vec(),
            chain(),
            0,
            0,
        )
        .expect("offer material");
        context.activate_for_test(material).expect("activate offer");
        context
    }

    fn mint_request(ctx: &InProcessTeeContext) -> PromisOpRequest {
        let account = Address::repeat_byte(0x11);
        let amount = U256::from(7u64);
        let state_key = resident_state_key(ctx, &PROMIS, STATE_EPOCH).expect("promis key");
        let keys = derive_account_keys(&state_key, account).expect("account keys");
        let operation = ModifyOperation {
            account,
            op: PromisOp::Mint,
            amount,
            op_nonce: 0,
            chain_id: chain(),
        };
        PromisOpRequest {
            op: PromisOp::Mint,
            chain_id: chain(),
            account,
            amount,
            current_balance: Vec::new(),
            modify_auth: ModifyAuth {
                mac: modify_mac(&keys, &operation),
                op_nonce: 0,
            },
        }
    }

    #[test]
    fn execute_rejects_a_foreign_hash_before_the_signature() {
        let ctx = context(4);
        let request = mint_request(&ctx);
        let expected = promis_op_canonical_hash(&request);
        assert!(matches!(
            execute_promis(&ctx, B256::ZERO, &request),
            Err(TransportError::UnexpectedResponse)
        ));
        let signed = execute_promis(&ctx, expected, &request).expect("signed mint");
        let mut corrupt = signed.clone();
        corrupt.attestation_tag[0] ^= 0xff;
        assert!(matches!(
            authenticate_promis(expected, &ctx.attestation_public(), &corrupt),
            Err(TransportError::AppAttestation { .. })
        ));
        corrupt.attestation_tag = signed.attestation_tag.clone();
        corrupt.inputs_canonical_hash = B256::ZERO;
        assert!(matches!(
            authenticate_promis(expected, &ctx.attestation_public(), &corrupt),
            Err(TransportError::UnexpectedResponse)
        ));
    }

    #[test]
    fn finish_keeps_the_first_signature_after_the_global_context_is_replaced() {
        let first = context(4);
        let second = context(9);
        assert_ne!(first.attestation_public(), second.attestation_public());
        let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&first)));
        let request = mint_request(&first);
        let expected = promis_op_canonical_hash(&request);
        let signed = execute_promis(&first, expected, &request).expect("signed mint");
        let _successor = set_in_process_tee_context_for_test(Some(second.clone()));
        assert_eq!(
            outbe_tee::in_process_tee_context()
                .expect("successor")
                .attestation_public(),
            second.attestation_public()
        );
        assert!(authenticate_promis(expected, &second.attestation_public(), &signed).is_err());
        assert!(finish_promis(expected, signed).is_ok());
    }
}
