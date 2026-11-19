//! Host caller for confidential Promis writes and account-key delivery.
//!
//! `ApplyPromisOp` and `MineGratisFromPromisV1` are witness-recorded. The
//! inputs hash is fixed before the call and checked again inside `execute`
//! against the same context that signed. A foreign chain is a signed modify-MAC
//! rejection, not a host chain check. `outbe_derivePromisKeys` is session-only.

use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::{
    consensus_witness_call, session_witness_call, FIDELITY, GRATIS, PROMIS,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::{CallMeta, TransportError};
use thiserror::Error;

use crate::attest::{verify_mine_gratis_from_promis_attestation, verify_promis_op_attestation};
use crate::engine::{apply_mine_gratis, apply_op, MineGratisKeys};
use crate::errors::{map_consensus_error, PromisFatalError};
use crate::protocol::{
    mine_gratis_from_promis_attestation_preimage, mine_gratis_from_promis_canonical_hash,
    promis_op_attestation_preimage, promis_op_canonical_hash, MineGratisFromPromisRequestV1,
    MineGratisFromPromisResultV1, PromisOpRequest, PromisOpResult,
};
use crate::resident::resident_state_key;
use crate::witness::MineGratisFromPromisBody;

const APPLY_OP: &str = "ApplyPromisOp";
const MINE_OP: &str = "MineGratisFromPromisV1";
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

/// Burn Promis and mint Gratis. A business rejection stays inside the result.
pub fn mine_gratis_from_promis(
    req: MineGratisFromPromisRequestV1,
) -> Result<MineGratisFromPromisResultV1> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_mine_gratis(&req) {
        return Ok(result);
    }
    let expected = mine_gratis_from_promis_canonical_hash(&req);
    let result = consensus_witness_call(
        CallMeta::recorded(MINE_OP),
        |context| execute_mine(context, expected, &req),
        |key, result| authenticate_mine(expected, key, result),
    )
    .map_err(|error| map_consensus_error(MINE_OP, error))?;
    finish_mine(expected, result)
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

fn execute_mine(
    context: &InProcessTeeContext,
    expected: B256,
    req: &MineGratisFromPromisRequestV1,
) -> std::result::Result<MineGratisFromPromisBody, TransportError> {
    let promis = resident_state_key(context, &PROMIS, STATE_EPOCH)?;
    let gratis = resident_state_key(context, &GRATIS, STATE_EPOCH)?;
    let fidelity = resident_state_key(context, &FIDELITY, STATE_EPOCH)?;
    let mut result = context
        .with_offer_key_handle(|offer_secret| {
            apply_mine_gratis(
                MineGratisKeys {
                    offer_secret,
                    promis_state_key: &promis,
                    gratis_state_key: &gratis,
                    fidelity_state_key: &fidelity,
                },
                req,
            )
        })
        .map_err(|err| -> TransportError {
            outbe_tee::InProcessDispatchError::Resident(err).into()
        })?;
    let preimage =
        mine_gratis_from_promis_attestation_preimage(result.inputs_canonical_hash, &result)
            .map_err(TransportError::codec)?;
    result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
    let body = MineGratisFromPromisBody(result);
    authenticate_mine(expected, &context.attestation_public(), &body)?;
    Ok(body)
}

fn execute_derive(
    context: &InProcessTeeContext,
    request: &DerivePromisKeysRequest,
) -> std::result::Result<SealedPromisKeys, TransportError> {
    let signature: [u8; 65] = request
        .owner_sig
        .as_slice()
        .try_into()
        .map_err(|_| TransportError::enclave_error(DeriveKeysError::OwnerSignatureLength))?;
    let state_key = resident_state_key(context, &PROMIS, STATE_EPOCH)?;
    let keys = PROMIS
        .account_keys
        .derive_account_keys(&state_key, request.account)
        .map_err(TransportError::enclave_error)?;
    let sealed = PROMIS
        .export_sealed_account_keys(&keys, &signature, &request.requester_ephemeral_pubkey)
        .map_err(TransportError::enclave_error)?;
    Ok(SealedPromisKeys {
        account: request.account,
        sealed: sealed.sealed,
        nonce: sealed.nonce,
        enclave_ephemeral_pubkey: sealed.enclave_ephemeral_pubkey,
    })
}

fn authenticate_promis(
    expected: B256,
    key: &[u8; 32],
    result: &PromisOpResult,
) -> std::result::Result<(), TransportError> {
    if result.inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    verify_promis_op_attestation(
        key,
        result.inputs_canonical_hash,
        result,
        &result.attestation_tag,
    )
}

fn authenticate_mine(
    expected: B256,
    key: &[u8; 32],
    result: &MineGratisFromPromisBody,
) -> std::result::Result<(), TransportError> {
    if result.0.inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    verify_mine_gratis_from_promis_attestation(
        key,
        result.0.inputs_canonical_hash,
        &result.0,
        &result.0.attestation_tag,
    )
}

fn finish_promis(expected: B256, result: PromisOpResult) -> Result<PromisOpResult> {
    if result.inputs_canonical_hash != expected {
        return Err(PromisFatalError::EnclaveNondeterminism.into());
    }
    Ok(result)
}

fn finish_mine(
    expected: B256,
    result: MineGratisFromPromisBody,
) -> Result<MineGratisFromPromisResultV1> {
    if result.0.inputs_canonical_hash != expected {
        return Err(PromisFatalError::EnclaveNondeterminism.into());
    }
    Ok(result.0)
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
    use outbe_core_common::confidential_tee::encrypt_tee_amount;
    use outbe_primitives::chain::DEVNET_CHAIN_ID;
    use std::cell::RefCell;

    thread_local! {
        static STATE_KEY: RefCell<Option<outbe_tee::resident::StateKeyHandle>> = const { RefCell::new(None) };
    }

    const DEV_EPOCH: u64 = 0;
    const PROMIS_GROUP_SIG: &[u8] = b"outbe-dev-promis-group-signature-fixed-seed!!";
    const PROMIS_CHAIN: B256 = B256::repeat_byte(0xC1);
    const GRATIS_GROUP_SIG: &[u8] = b"outbe-dev-gratis-group-signature-fixed-seed!!";
    const GRATIS_CHAIN: B256 = B256::repeat_byte(0xC1);
    const FIDELITY_GROUP_SIG: &[u8] = b"outbe-dev-fidelity-group-signature-fixed-seed";
    const OFFER_SECRET: [u8; 32] = [0x42; 32];

    fn offer(group: &[u8], chain: B256) -> outbe_tee::resident::ResidentOfferMaterial {
        outbe_tee::resident::ResidentOfferMaterial::for_test(group.to_vec(), chain, DEV_EPOCH, 0)
            .expect("dev offer material")
    }

    fn fidelity_chain() -> B256 {
        B256::from(U256::from(DEVNET_CHAIN_ID))
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

    /// Encrypt a `mineGratis` amount to the stand-in offer key. The chain id is 1.
    pub fn encrypt_mine_gratis_amount(account: Address, amount: U256) -> Vec<u8> {
        let public = outbe_tee::resident::OfferKeyHandle::for_test(OFFER_SECRET).public();
        encrypt_tee_amount(&public, B256::from(U256::from(1u64)), account, amount)
            .expect("encrypt mine_gratis amount")
            .to_vec()
    }

    pub(crate) fn try_apply(req: &PromisOpRequest) -> Option<PromisOpResult> {
        STATE_KEY.with(|slot| slot.borrow().as_ref().map(|key| apply_op(key, req)))
    }

    pub(crate) fn try_mine_gratis(
        req: &MineGratisFromPromisRequestV1,
    ) -> Option<MineGratisFromPromisResultV1> {
        STATE_KEY.with(|slot| {
            slot.borrow().as_ref().map(|promis_state_key| {
                let gratis_material = offer(GRATIS_GROUP_SIG, GRATIS_CHAIN);
                let gratis_state_key = outbe_gratis::derive_gratis_state_key(
                    &gratis_material,
                    GRATIS_CHAIN,
                    DEV_EPOCH,
                )
                .expect("derive dev gratis state key");
                let fidelity_material = offer(FIDELITY_GROUP_SIG, fidelity_chain());
                let fidelity_state_key = outbe_fidelity::derive_fidelity_state_key(
                    &fidelity_material,
                    fidelity_chain(),
                    DEV_EPOCH,
                )
                .expect("derive dev fidelity state key");
                let offer_secret = outbe_tee::resident::OfferKeyHandle::for_test(OFFER_SECRET);
                apply_mine_gratis(
                    MineGratisKeys {
                        offer_secret: &offer_secret,
                        promis_state_key,
                        gratis_state_key: &gratis_state_key,
                        fidelity_state_key: &fidelity_state_key,
                    },
                    req,
                )
            })
        })
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
