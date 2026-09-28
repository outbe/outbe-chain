//! Synchronous Gratis client over the separate confidential state journals.
//! The shared client binds the request/response to committed heads and verifies
//! the local enclave attestation; only ciphertext updates enter chain storage.

use outbe_primitives::error::Result;
#[cfg(any(test, feature = "test-enclave"))]
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};

/// Execute against committed heads, recovering disposable enclave caches as needed.
pub(crate) fn execute(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    call: outbe_tee::confidential::Call,
) -> Result<outbe_tee::confidential::Applied> {
    outbe_tee::confidential::execute(storage, call, |request| {
        #[cfg(any(test, feature = "test-enclave"))]
        {
            test_enclave::try_confidential(request)
        }
        #[cfg(not(any(test, feature = "test-enclave")))]
        {
            let _ = request;
            None
        }
    })
}

/// In-process enclave stand-in for tests (this crate's tests and any downstream
/// crate that enables the `test-enclave` feature). It runs the **real**
/// `outbe_tee_enclave::gratis::apply_op` engine against a fixed dev state key, so
/// the full confidential path is exercised without an SGX sidecar. Attestation is
/// not checked on this path (it is verified only in the mock-enclave e2e).
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

    pub(crate) fn try_confidential(req: &EnclaveRequest) -> Option<EnclaveResponse> {
        STATE_KEY.with(|k| {
            k.borrow().map(|key| {
                let fidelity_key = outbe_tee_enclave::fidelity::derive_fidelity_state_key(
                    outbe_tee_enclave::dev::FIDELITY_GROUP_SIG,
                    outbe_tee_enclave::dev::fidelity_chain(),
                    DEV_EPOCH,
                )
                .expect("dev fidelity key");
                EnclaveResponse::Confidential {
                    response: outbe_tee_enclave::confidential_ledger::dispatch(
                        req,
                        &key,
                        &fidelity_key,
                        &outbe_tee_enclave::dev::CREDENTIAL_SECRET,
                    ),
                }
            })
        })
    }
}
