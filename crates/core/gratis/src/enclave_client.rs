//! Host caller for confidential Gratis writes and account-key delivery.
//!
//! `ApplyGratisOp` and `MineEncryptedNodV1` are witness-recorded. The inputs
//! hash is fixed before the call and checked again inside `execute` against
//! the same context that signed. `outbe_deriveGratisKeys` is session-only.

use std::fmt::Write as _;

use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::{
    consensus_witness_call, seal_resident_account_keys, session_witness_call,
    verify_bound_witness_attestation, FIDELITY, GRATIS,
};
use outbe_fidelity::apply_cohort_section;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::{CallMeta, TransportError};
use thiserror::Error;

use crate::attest::verify_gratis_op_attestation;
use crate::engine::{apply_op, rejected_result};
use crate::errors::{map_consensus_error, GratisFatalError};
use crate::nod_mine::{self, MineEncryptedNodRequestV1, MineEncryptedNodResultV1};
use crate::protocol::{
    gratis_op_attestation_preimage, gratis_op_canonical_hash, GratisOpRequest, GratisOpResult,
    GratisOpStatus,
};
use crate::resident::resident_state_key;

const APPLY_OP: &str = "ApplyGratisOp";
const DERIVE_OP: &str = "outbe_deriveGratisKeys";
const STATE_EPOCH: u64 = 0;

/// Sealed Gratis view and modify keys for `outbe_deriveGratisKeys`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SealedGratisKeys {
    /// Account the keys belong to.
    pub account: Address,
    /// Ciphertext sealed to the caller's ephemeral key.
    pub sealed: Vec<u8>,
    /// AEAD nonce.
    pub nonce: [u8; 12],
    /// Enclave X25519 ephemeral public key.
    pub enclave_ephemeral_pubkey: [u8; 32],
}

/// Owner-authorized request for [`derive_gratis_keys`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeriveGratisKeysRequest {
    /// Account whose keys are sealed.
    pub account: Address,
    /// Caller X25519 public key.
    pub requester_ephemeral_pubkey: [u8; 32],
    /// 65-byte owner signature over the Gratis derive-keys message.
    pub owner_sig: Vec<u8>,
}

#[derive(Debug, Error)]
enum DeriveKeysError {
    #[error("owner signature must be 65 bytes")]
    OwnerSignatureLength,
}

/// Enclave witness one collateral op (pledge, release, pledge burn) records at most; it carries no fidelity section.
pub const COLLATERAL_OP_WITNESS_BYTES: u32 = 460;

/// Run one Gratis op. A business rejection stays inside the returned result.
pub fn apply_gratis_op(req: GratisOpRequest) -> Result<GratisOpResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_apply(&req) {
        return Ok(result);
    }
    let expected = gratis_op_canonical_hash(&req);
    let result = consensus_witness_call(
        CallMeta::recorded(APPLY_OP),
        |context| execute_apply(context, expected, &req),
        |key, result| authenticate_apply(expected, key, result),
    )
    .map_err(|error| map_consensus_error(APPLY_OP, error))?;
    finish_apply(expected, result)
}

/// Mine Gratis from an encrypted NOD. A signed refusal is [`crate::errors::GratisError::Rejected`].
pub fn mine_encrypted_nod(request: MineEncryptedNodRequestV1) -> Result<MineEncryptedNodResultV1> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_mine(&request) {
        return result;
    }
    nod_mine::witnessed(request)
}

/// Seal the account's Gratis keys. This call is not written to the witness tape.
pub fn derive_gratis_keys(request: DeriveGratisKeysRequest) -> Result<SealedGratisKeys> {
    let sealed = session_witness_call(CallMeta::session_only(DERIVE_OP), |context| {
        execute_derive(context, &request)
    })
    .ok_or(GratisFatalError::SidecarUnavailable { context: DERIVE_OP })?
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

fn execute_apply(
    context: &InProcessTeeContext,
    expected: B256,
    req: &GratisOpRequest,
) -> std::result::Result<GratisOpResult, TransportError> {
    let key = resident_state_key(context, &GRATIS, STATE_EPOCH)?;
    let mut result = apply_op(&key, req);
    if let (GratisOpStatus::Applied, Some(section)) = (&result.status, &req.fidelity) {
        let fidelity_key = resident_state_key(context, &FIDELITY, STATE_EPOCH)?;
        match apply_cohort_section(&fidelity_key, req.account, req.amount, section) {
            Ok(outcome) => result.fidelity = Some(outcome),
            Err(error) => result = rejected_result(fidelity_reason(&error), expected),
        }
    }
    let preimage = gratis_op_attestation_preimage(result.inputs_canonical_hash, &result)
        .map_err(TransportError::codec)?;
    result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
    authenticate_apply(expected, &context.attestation_public(), &result)?;
    Ok(result)
}

fn fidelity_reason(error: &impl std::fmt::Display) -> String {
    let mut reason = String::from("fidelity section failed: ");
    let _ = write!(&mut reason, "{error}");
    reason
}

impl From<(Address, outbe_tee::resident::SealedAccountKeysExport)> for SealedGratisKeys {
    fn from((account, s): (Address, outbe_tee::resident::SealedAccountKeysExport)) -> Self {
        Self {
            account,
            sealed: s.sealed,
            nonce: s.nonce,
            enclave_ephemeral_pubkey: s.enclave_ephemeral_pubkey,
        }
    }
}

fn execute_derive(
    context: &InProcessTeeContext,
    req: &DeriveGratisKeysRequest,
) -> std::result::Result<SealedGratisKeys, TransportError> {
    let input = (
        req.account,
        req.owner_sig.as_slice(),
        &req.requester_ephemeral_pubkey,
    );
    seal_resident_account_keys(
        context,
        (&GRATIS, STATE_EPOCH),
        input,
        || DeriveKeysError::OwnerSignatureLength,
        |a, s| SealedGratisKeys::from((a, s)),
    )
}

fn authenticate_apply(
    expected: B256,
    key: &[u8; 32],
    result: &GratisOpResult,
) -> std::result::Result<(), TransportError> {
    verify_bound_witness_attestation(expected, key, result, verify_gratis_op_attestation)
}

fn finish_apply(expected: B256, result: GratisOpResult) -> Result<GratisOpResult> {
    if result.inputs_canonical_hash != expected {
        return Err(GratisFatalError::EnclaveNondeterminism.into());
    }
    Ok(result)
}

/// In-process stand-in. It runs the real engine and skips attestation.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use super::*;
    use crate::engine::{
        derive_account_keys, derive_gratis_state_key, modify_mac, ModifyOperation,
    };
    use crate::protocol::ModifyAuth;
    use std::cell::RefCell;

    thread_local! {
        static STATE_KEY: RefCell<Option<outbe_tee::resident::StateKeyHandle>> = const { RefCell::new(None) };
    }

    use outbe_core_common::confidential_tee::{
        fidelity_chain, FIDELITY_GROUP_SIG, GRATIS_CHAIN, GRATIS_GROUP_SIG,
    };

    const DEV_EPOCH: u64 = 0;

    fn gratis_chain() -> B256 {
        GRATIS_CHAIN
    }

    fn offer(
        group: &[u8],
        chain: B256,
    ) -> std::result::Result<outbe_tee::resident::ResidentOfferMaterial, GratisFatalError> {
        outbe_tee::resident::ResidentOfferMaterial::for_test(group.to_vec(), chain, DEV_EPOCH, 0)
            .map_err(|error| GratisFatalError::TestEnclave {
                source: Box::new(error),
            })
    }

    fn fixture_key() -> outbe_tee::resident::StateKeyHandle {
        let material = offer(GRATIS_GROUP_SIG, gratis_chain()).expect("dev gratis offer material");
        derive_gratis_state_key(&material, gratis_chain(), DEV_EPOCH)
            .expect("derive dev gratis state key")
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

    /// Authorization bound to the installed fixture key.
    pub fn modify_auth(operation: ModifyOperation) -> ModifyAuth {
        let key = derive_account_keys(&state_key(), operation.account).expect("account keys");
        ModifyAuth {
            mac: modify_mac(&key, &operation),
            op_nonce: operation.op_nonce,
        }
    }

    pub(crate) fn try_mine(
        request: &MineEncryptedNodRequestV1,
    ) -> Option<Result<MineEncryptedNodResultV1>> {
        STATE_KEY.with(|slot| {
            slot.borrow().as_ref().map(|gratis| {
                let fidelity = derive_fidelity_key()?;
                let nod = outbe_tee::resident::StateKeyHandle::for_test([0x5a; 32]);
                nod_mine::apply(
                    nod_mine::MineKeys {
                        nod: &nod,
                        gratis,
                        fidelity: &fidelity,
                    },
                    request,
                )
                .map_err(nod_mine::map_stand_in)
            })
        })
    }

    pub(crate) fn try_apply(req: &GratisOpRequest) -> Option<GratisOpResult> {
        STATE_KEY.with(|slot| {
            slot.borrow().as_ref().map(|key| {
                let mut result = apply_op(key, req);
                if let (GratisOpStatus::Applied, Some(section)) = (&result.status, &req.fidelity) {
                    let fidelity = derive_fidelity_key().expect("derive dev fidelity state key");
                    match apply_cohort_section(&fidelity, req.account, req.amount, section) {
                        Ok(outcome) => result.fidelity = Some(outcome),
                        Err(error) => {
                            result = rejected_result(
                                fidelity_reason(&error),
                                result.inputs_canonical_hash,
                            );
                        }
                    }
                }
                result
            })
        })
    }

    fn derive_fidelity_key() -> Result<outbe_tee::resident::StateKeyHandle> {
        let material = offer(FIDELITY_GROUP_SIG, fidelity_chain())?;
        outbe_fidelity::derive_fidelity_state_key(&material, fidelity_chain(), DEV_EPOCH).map_err(
            |error| {
                GratisFatalError::TestEnclave {
                    source: Box::new(error),
                }
                .into()
            },
        )
    }
}

#[cfg(test)]
mod executing_context {
    use super::*;
    use crate::engine::{derive_account_keys, modify_mac, ModifyOperation};
    use crate::protocol::GratisOp;
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
            b"gratis-witness-group-signature-v1!!!!!".to_vec(),
            chain(),
            0,
            0,
        )
        .expect("offer material");
        context.activate_for_test(material).expect("activate offer");
        context
    }

    fn mint_request(ctx: &InProcessTeeContext) -> GratisOpRequest {
        let account = Address::repeat_byte(0x11);
        let amount = U256::from(7u64);
        let state_key = resident_state_key(ctx, &GRATIS, STATE_EPOCH).expect("gratis key");
        let keys = derive_account_keys(&state_key, account).expect("account keys");
        let operation = ModifyOperation {
            account,
            op: GratisOp::Mint,
            amount,
            op_nonce: 0,
            chain_id: chain(),
        };
        GratisOpRequest {
            op: GratisOp::Mint,
            chain_id: chain(),
            account,
            amount,
            current_balance: Vec::new(),
            current_pledged: Vec::new(),
            modify_auth: crate::protocol::ModifyAuth {
                mac: modify_mac(&keys, &operation),
                op_nonce: 0,
            },
            fidelity: None,
        }
    }

    #[test]
    fn execute_rejects_a_foreign_hash_before_the_signature() {
        let ctx = context(4);
        let request = mint_request(&ctx);
        let expected = gratis_op_canonical_hash(&request);
        assert!(matches!(
            execute_apply(&ctx, B256::ZERO, &request),
            Err(TransportError::UnexpectedResponse)
        ));
        let signed = execute_apply(&ctx, expected, &request).expect("signed mint");
        let mut corrupt = signed.clone();
        corrupt.attestation_tag[0] ^= 0xff;
        assert!(matches!(
            authenticate_apply(expected, &ctx.attestation_public(), &corrupt),
            Err(TransportError::AppAttestation { .. })
        ));
        corrupt.attestation_tag = signed.attestation_tag.clone();
        corrupt.inputs_canonical_hash = B256::ZERO;
        assert!(matches!(
            authenticate_apply(expected, &ctx.attestation_public(), &corrupt),
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
        let expected = gratis_op_canonical_hash(&request);
        let signed = execute_apply(&first, expected, &request).expect("signed mint");
        let _successor = set_in_process_tee_context_for_test(Some(second.clone()));
        assert_eq!(
            outbe_tee::in_process_tee_context()
                .expect("successor")
                .attestation_public(),
            second.attestation_public()
        );
        assert!(authenticate_apply(expected, &second.attestation_public(), &signed).is_err());
        assert!(finish_apply(expected, signed).is_ok());
    }
}
