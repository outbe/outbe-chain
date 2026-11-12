//! Host-side enclave client for the confidential Promis write path.
//!
//! Every Promis state transition routes through the enclave. [`crate::runtime`] does these
//! steps:
//!
//! 1. Read the current ciphertext from committed storage.
//! 2. Hand it and the op to the enclave via [`apply_promis_op`].
//! 3. Store the returned ciphertext verbatim.
//!
//! This module mirrors `gratis::enclave_client`. It has the same determinism (canonical-hash
//! recheck) and attestation (verify-then-discard) guarantees. It has the same
//! `tee_sidecar_unavailable` failure mode when no enclave is configured.

use outbe_primitives::error::Result;
use outbe_tee::protocol::{
    promis_op_canonical_hash, EnclaveRequest, EnclaveResponse, MineGratisFromPromisRequestV1,
    MineGratisFromPromisResultV1, PromisOpRequest, PromisOpResult,
};
use outbe_tee::TransportError;

use crate::errors::PromisFatalError;

/// Run one Promis op inside the enclave and validate the response.
///
/// Determinism: recompute the canonical inputs hash and reject a mismatch
/// (`tee_enclave_nondeterminism`). Attestation: verify the tag against the enclave
/// key pinned from its quote (`tee_promis_attestation_invalid`), then discard it.
/// Nothing writes it to state. A missing enclave is `tee_sidecar_unavailable`.
/// All of these are `Fatal` (a node/consensus fault, not a user revert). A
/// *business* rejection is in `PromisOpResult::status`, and the caller handles
/// it.
pub(crate) fn apply_promis_op(req: PromisOpRequest) -> Result<PromisOpResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_apply(&req) {
        return Ok(result);
    }

    let expected_hash = promis_op_canonical_hash(&req);
    let (attestation_pub, response) = outbe_tee::consensus_call(
        "ApplyPromisOp",
        || {
            outbe_tee::try_with_enclave(|client| {
                let attestation_pub = client.attestation_pub();
                client
                    .request(&EnclaveRequest::ApplyPromisOp {
                        request: Box::new(req),
                    })
                    .map(|response| (attestation_pub, response))
            })
            .ok_or_else(|| TransportError::Unavailable("enclave session unavailable".into()))?
        },
        |key, response| match response {
            EnclaveResponse::PromisOpApplied { result } => {
                if result.inputs_canonical_hash != expected_hash {
                    return Err(TransportError::UnexpectedResponse);
                }
                outbe_tee::verify_promis_op_attestation(
                    key,
                    result.inputs_canonical_hash,
                    result,
                    &result.attestation_tag,
                )
            }
            _ => Err(TransportError::UnexpectedResponse),
        },
    )
    .map_err(|error| match error {
        TransportError::EnclaveError(message) => PromisFatalError::EnclaveOpError(message),
        other => PromisFatalError::sidecar_request_failed(other),
    })?;

    let result = match response {
        EnclaveResponse::PromisOpApplied { result } => *result,
        EnclaveResponse::Error { message } => {
            return Err(PromisFatalError::EnclaveOpError(message).into())
        }
        other => return Err(PromisFatalError::UnexpectedEnclaveResponse(Box::new(other)).into()),
    };

    if result.inputs_canonical_hash != expected_hash {
        return Err(PromisFatalError::EnclaveNondeterminism.into());
    }
    outbe_tee::verify_promis_op_attestation(
        &attestation_pub,
        result.inputs_canonical_hash,
        &result,
        &result.attestation_tag,
    )
    .map_err(PromisFatalError::attestation_invalid)?;
    Ok(result)
}

/// Run the combined Promis->Gratis conversion inside the enclave and validate the response.
pub(crate) fn mine_gratis_from_promis(
    req: MineGratisFromPromisRequestV1,
) -> Result<MineGratisFromPromisResultV1> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(res) = test_enclave::try_mine_gratis(&req) {
        return Ok(res);
    }

    let request = EnclaveRequest::MineGratisFromPromisV1 {
        request: Box::new(req),
    };
    let response = outbe_tee::balance_client::execute_confidential_balance_op(request)?;
    let EnclaveResponse::GratisMinedFromPromisV1 { result } = response else {
        return Err(PromisFatalError::UnexpectedEnclaveResponse(Box::new(response)).into());
    };
    Ok(*result)
}

/// In-process enclave stand-in for tests (this crate's tests and any downstream
/// crate that enables the `test-enclave` feature). It runs the **real**
/// `outbe_enclave::promis::apply_op` engine against a fixed dev state key, so
/// tests exercise the full confidential path without an SGX sidecar. This path
/// does not check attestation. The crate root rejects
/// `all(feature = "test-enclave", feature = "production-enclave")` at compile
/// time and CI's `no-test-deps` gate rejects `test-enclave` in the production
/// dependency graph, so this stand-in cannot reach a production build.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use super::*;
    use alloy_primitives::B256;
    use std::cell::RefCell;

    thread_local! {
        static STATE_KEY: RefCell<Option<[u8; 32]>> = const { RefCell::new(None) };
    }

    /// Fixed dev group signature + chain/epoch, so the derived state key (and thus
    /// every account's view/modify key) is deterministic across a test process.
    const DEV_GROUP_SIG: &[u8] = b"outbe-dev-promis-group-signature-fixed-seed!!";
    const DEV_CHAIN: B256 = B256::repeat_byte(0xC1);
    const DEV_EPOCH: u64 = 0;

    /// Install the in-process enclave for the current thread.
    pub fn install() {
        let key =
            outbe_enclave::promis::derive_promis_state_key(DEV_GROUP_SIG, DEV_CHAIN, DEV_EPOCH)
                .expect("derive dev promis state key");
        STATE_KEY.with(|k| *k.borrow_mut() = Some(*key));
    }

    /// Remove the in-process enclave for the current thread.
    pub fn uninstall() {
        STATE_KEY.with(|k| *k.borrow_mut() = None);
    }

    /// The modify authorization a client would send for `op` on chain 1.
    pub fn modify_auth(
        op: outbe_tee::protocol::PromisOp,
        account: alloy_primitives::Address,
        amount: alloy_primitives::U256,
        nonce: u64,
    ) -> outbe_tee::protocol::ModifyAuth {
        use outbe_enclave::promis::{derive_modify_key, modify_mac, ModifyOperation};
        let mk = derive_modify_key(&state_key(), account).unwrap();
        outbe_tee::protocol::ModifyAuth {
            mac: modify_mac(
                &mk,
                &ModifyOperation {
                    account,
                    op,
                    amount,
                    op_nonce: nonce,
                    chain_id: B256::from(alloy_primitives::U256::from(1u64)),
                },
            ),
            op_nonce: nonce,
        }
    }

    /// Encrypt a `mineGratis` amount to the test-enclave's dev TEE public key.
    /// The test chain id is `1`, matching [`crate::runtime`]'s dev storage.
    pub fn encrypt_mine_gratis_amount(
        account: alloy_primitives::Address,
        amount: alloy_primitives::U256,
    ) -> Vec<u8> {
        outbe_tee::offer_encrypt::encrypt_tee_amount(
            &outbe_enclave::dev::offer_public_key(),
            B256::from(alloy_primitives::U256::from(1u64)),
            account,
            amount,
        )
        .expect("encrypt mine_gratis amount")
        .to_vec()
    }

    /// The dev state key, so tests can derive view/modify keys to build auth and
    /// decrypt balances exactly as a client would.
    pub fn state_key() -> [u8; 32] {
        STATE_KEY
            .with(|k| *k.borrow())
            .expect("test enclave not installed")
    }

    pub(crate) fn try_apply(req: &PromisOpRequest) -> Option<PromisOpResult> {
        STATE_KEY.with(|k| {
            k.borrow()
                .map(|key| outbe_enclave::promis::apply_op(&key, req))
        })
    }

    pub(crate) fn try_mine_gratis(
        req: &MineGratisFromPromisRequestV1,
    ) -> Option<MineGratisFromPromisResultV1> {
        STATE_KEY.with(|k| {
            k.borrow().map(|promis_state_key| {
                let gratis_state_key = outbe_enclave::gratis::derive_gratis_state_key(
                    outbe_enclave::dev::GRATIS_GROUP_SIG,
                    outbe_enclave::dev::GRATIS_CHAIN,
                    DEV_EPOCH,
                )
                .expect("derive dev gratis state key");
                let fidelity_state_key = outbe_enclave::fidelity::derive_fidelity_state_key(
                    outbe_enclave::dev::FIDELITY_GROUP_SIG,
                    outbe_enclave::dev::fidelity_chain(),
                    DEV_EPOCH,
                )
                .expect("derive dev fidelity state key");
                let keys = outbe_enclave::promis::MineGratisKeys {
                    offer_secret: &outbe_enclave::dev::OFFER_SECRET_KEY,
                    promis_state_key: &promis_state_key,
                    gratis_state_key: &gratis_state_key,
                    fidelity_state_key: &fidelity_state_key,
                };
                outbe_enclave::promis::apply_mine_gratis(keys, req)
            })
        })
    }
}
