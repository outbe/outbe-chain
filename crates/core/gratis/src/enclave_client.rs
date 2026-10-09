//! Host-side enclave client for the confidential Gratis write path.
//!
//! Every Gratis state transition goes through the enclave. [`crate::runtime`] does
//! these steps:
//!
//! 1. It reads the current ciphertext from committed storage.
//! 2. It sends the ciphertext and the op to the enclave through [`apply_gratis_op`].
//! 3. It stores the returned ciphertext verbatim.
//!
//! This module mirrors `tributefactory::enclave_offer`. It gives the same determinism
//! guarantee (canonical-hash recheck) and the same attestation guarantee
//! (verify-then-discard). It also has the same `tee_sidecar_unavailable` failure mode
//! when no enclave is configured.

use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse, GratisOpRequest, GratisOpResult};

/// Run one Gratis op inside the enclave and validate the response.
///
/// Determinism: recompute the canonical inputs hash and reject a mismatch
/// (`tee_enclave_nondeterminism`). Attestation: verify the tag against the
/// enclave key pinned from its quote (`tee_gratis_attestation_invalid`), then
/// discard the tag. The tag is never written to state. A missing or not-ready
/// enclave is `EnclaveUnavailable`. The other errors are `Fatal` (a node/consensus
/// fault, not a user revert). `GratisOpResult::status` carries a *business*
/// rejection, and the caller handles it.
pub(crate) fn apply_gratis_op(req: GratisOpRequest) -> Result<GratisOpResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_apply(&req) {
        return Ok(result);
    }

    match outbe_tee::balance_client::execute_confidential_balance_op(
        EnclaveRequest::ApplyGratisOp {
            request: Box::new(req),
        },
    )? {
        EnclaveResponse::GratisOpApplied { result } => Ok(*result),
        other => Err(PrecompileError::Fatal(format!(
            "unexpected enclave response: {other:?}"
        ))),
    }
}

/// In-process enclave stand-in for tests (this crate's tests and any downstream
/// crate that enables the `test-enclave` feature). It runs the **real**
/// `outbe_tee_enclave::gratis::apply_op` engine against a fixed dev state key. Thus
/// tests exercise the full confidential path without an SGX sidecar. This path does
/// not check attestation. Only the mock-enclave e2e verifies it.
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
    const DEV_GROUP_SIG: &[u8] = b"outbe-dev-gratis-group-signature-fixed-seed!!";
    const DEV_CHAIN: B256 = B256::repeat_byte(0xC1);
    const DEV_EPOCH: u64 = 0;

    /// Install the in-process enclave for the current thread.
    pub fn install() {
        let key =
            outbe_tee_enclave::gratis::derive_gratis_state_key(DEV_GROUP_SIG, DEV_CHAIN, DEV_EPOCH)
                .expect("derive dev gratis state key");
        STATE_KEY.with(|k| *k.borrow_mut() = Some(key));
    }

    /// Remove the in-process enclave for the current thread.
    pub fn uninstall() {
        STATE_KEY.with(|k| *k.borrow_mut() = None);
    }

    /// The dev state key, so tests can derive view/modify keys to build auth and
    /// decrypt balances exactly as a client would.
    pub fn state_key() -> [u8; 32] {
        STATE_KEY
            .with(|k| *k.borrow())
            .expect("test enclave not installed")
    }

    /// Builds an authorization with the installed fixture key and operation nonce.
    pub fn modify_auth(
        operation: outbe_tee_enclave::gratis::ModifyOperation,
    ) -> outbe_tee::protocol::ModifyAuth {
        let key =
            outbe_tee_enclave::gratis::derive_modify_key(&state_key(), operation.account).unwrap();
        outbe_tee::protocol::ModifyAuth {
            mac: outbe_tee_enclave::gratis::modify_mac(&key, &operation),
            op_nonce: operation.op_nonce,
        }
    }

    pub(crate) fn try_mine(
        request: &outbe_tee::nod_mine::MineEncryptedNodRequestV2,
    ) -> Option<Result<outbe_tee::nod_mine::MineEncryptedNodResultV2>> {
        STATE_KEY.with(|key| {
            key.borrow().map(|state_key| {
                let fidelity_key = outbe_tee_enclave::fidelity::derive_fidelity_state_key(
                    outbe_tee_enclave::dev::FIDELITY_GROUP_SIG,
                    outbe_tee_enclave::dev::fidelity_chain(),
                    DEV_EPOCH,
                )
                .map_err(|error| PrecompileError::Fatal(error.to_string()))?;
                outbe_tee_enclave::nod_mine::apply(&[0x5a; 32], &state_key, &fidelity_key, request)
                    .map_err(|error| match error {
                        outbe_tee_enclave::errors::TeeError::TributeOfferReject(reason) => {
                            PrecompileError::Revert(reason)
                        }
                        other => PrecompileError::Fatal(other.to_string()),
                    })
            })
        })
    }

    pub(crate) fn try_apply(req: &GratisOpRequest) -> Option<GratisOpResult> {
        STATE_KEY.with(|k| {
            k.borrow().map(|key| {
                let mut result = outbe_tee_enclave::gratis::apply_op(&key, req);
                // Mirror the real transport's combined op. On success, apply the
                // co-located fidelity cohort section under the INDEPENDENT
                // fidelity key. That key is the shared dev fidelity identity, so a
                // folded mint writes a blob that the fidelity stand-in can later read.
                if let (outbe_tee::protocol::GratisOpStatus::Applied, Some(section)) =
                    (&result.status, &req.fidelity)
                {
                    let fidelity_key = outbe_tee_enclave::fidelity::derive_fidelity_state_key(
                        outbe_tee_enclave::dev::FIDELITY_GROUP_SIG,
                        outbe_tee_enclave::dev::fidelity_chain(),
                        DEV_EPOCH,
                    )
                    .expect("derive dev fidelity state key");
                    // Mirror the real transport. A failing fidelity section
                    // rejects the WHOLE op (rejected_result). The host then reverts
                    // and writes NEITHER ledger. The failure is not a panic.
                    match outbe_tee_enclave::fidelity::apply_cohort_section(
                        &fidelity_key,
                        req.account,
                        req.amount,
                        section,
                    ) {
                        Ok(outcome) => result.fidelity = Some(outcome),
                        Err(e) => {
                            result = outbe_tee_enclave::gratis::rejected_result(
                                format!("fidelity section failed: {e}"),
                                result.inputs_canonical_hash,
                            );
                        }
                    }
                }
                result
            })
        })
    }
}

pub(crate) fn mine_encrypted_nod(
    request: outbe_tee::nod_mine::MineEncryptedNodRequestV2,
) -> Result<outbe_tee::nod_mine::MineEncryptedNodResultV2> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_mine(&request) {
        return result;
    }
    outbe_tee::nod_mine::mine_encrypted_nod(request).map_err(|error| match error {
        outbe_tee::TransportError::NodMintRejected(reason) => PrecompileError::Revert(reason),
        other => PrecompileError::Fatal(format!("encrypted NOD enclave operation failed: {other}")),
    })
}
