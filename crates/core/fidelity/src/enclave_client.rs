//! Synchronous client for the independent confidential Fidelity journal.
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};
use outbe_tee::{
    confidential::{Call, Value},
    protocol::*,
};
fn execute(storage: &StorageHandle<'_>, call: Call) -> Result<Value> {
    let result = outbe_tee::confidential::execute(storage, call, |request| {
        #[cfg(any(test, feature = "test-enclave"))]
        {
            test_enclave::try_confidential(request)
        }
        #[cfg(not(any(test, feature = "test-enclave")))]
        {
            let _ = request;
            None
        }
    })?;
    storage.with_checkpoint(|| {
        for update in &result.updates {
            outbe_tee::confidential::persist(storage, update)?;
        }
        Ok(result.value)
    })
}
fn invalid() -> PrecompileError {
    PrecompileError::Fatal("invalid Fidelity result".into())
}
pub(crate) fn apply_cohort_op(
    storage: &StorageHandle<'_>,
    req: FidelityCohortRequest,
) -> Result<FidelityCohortResult> {
    match execute(storage, Call::Fidelity(Box::new(req)))? {
        Value::Fidelity(outcome) => Ok(FidelityCohortResult {
            outcome,
            inputs_canonical_hash: alloy_primitives::B256::ZERO,
            attestation_tag: Vec::new(),
        }),
        _ => Err(invalid()),
    }
}
pub(crate) fn snapshot_leagues(
    storage: &StorageHandle<'_>,
    req: FidelitySnapshotRequest,
) -> Result<Vec<FidelityLeagueEntry>> {
    match execute(storage, Call::FidelitySnapshot(Box::new(req)))? {
        Value::Snapshot(entries) => Ok(entries),
        _ => Err(invalid()),
    }
}
pub(crate) fn query_index(
    storage: &StorageHandle<'_>,
    req: FidelityQueryRequest,
) -> Result<FidelityQueryResult> {
    match execute(storage, Call::FidelityQuery(Box::new(req)))? {
        Value::Query(result) => Ok(result),
        _ => Err(invalid()),
    }
}

/// In-process enclave stand-in for tests (this crate's tests and any downstream
/// crate that enables the `test-enclave` feature). Runs the **real**
/// `outbe_tee_enclave::fidelity` engine against a fixed dev state key, so the
/// full confidential path is exercised without an SGX sidecar. Attestation is not
/// checked on this path (verified only in the mock-enclave e2e).
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use super::*;
    use alloy_primitives::B256;
    use std::cell::RefCell;

    thread_local! {
        static STATE_KEY: RefCell<Option<[u8; 32]>> = const { RefCell::new(None) };
    }

    const DEV_EPOCH: u64 = 0;

    /// Chain id the in-process enclave binds. Query auth is chain-scoped, so the
    /// resident chain must equal what the runtime derives from
    /// `storage.chain_id()` - tests that exercise the query path build their
    /// storage with this id and sign over [`dev_chain`]. Cohort/snapshot ops do
    /// not verify chain id, so downstream crates may use any storage chain id.
    ///
    /// The group sig + chain are the SHARED dev fidelity identity
    /// ([`outbe_tee_enclave::dev`]): the gratis stand-in derives the same
    /// fidelity key when it applies a folded cohort section, so a folded mint and
    /// a standalone snapshot agree.
    pub const DEV_CHAIN_ID: u64 = outbe_tee_enclave::dev::FIDELITY_CHAIN_ID;

    /// The resident chain id as a `B256`, matching the runtime's
    /// `B256::from(U256::from(storage.chain_id()))` encoding.
    pub fn dev_chain() -> B256 {
        outbe_tee_enclave::dev::fidelity_chain()
    }

    /// Install the in-process enclave for the current thread.
    pub fn install() {
        let key = outbe_tee_enclave::fidelity::derive_fidelity_state_key(
            outbe_tee_enclave::dev::FIDELITY_GROUP_SIG,
            dev_chain(),
            DEV_EPOCH,
        )
        .expect("derive dev fidelity state key");
        STATE_KEY.with(|k| *k.borrow_mut() = Some(key));
    }

    /// Remove the in-process enclave for the current thread.
    pub fn uninstall() {
        STATE_KEY.with(|k| *k.borrow_mut() = None);
    }

    /// The dev state key, so tests can derive view keys to decrypt cohorts.
    pub fn state_key() -> [u8; 32] {
        STATE_KEY
            .with(|k| *k.borrow())
            .expect("test enclave not installed")
    }

    pub(super) fn try_confidential(req: &EnclaveRequest) -> Option<EnclaveResponse> {
        STATE_KEY.with(|k| {
            k.borrow().map(|key| {
                let g = outbe_tee_enclave::gratis::derive_gratis_state_key(
                    b"outbe-dev-gratis-group-signature-fixed-seed!!",
                    B256::repeat_byte(0xC1),
                    0,
                )
                .expect("dev Gratis key");
                EnclaveResponse::Confidential {
                    response: outbe_tee_enclave::confidential_ledger::dispatch(
                        req,
                        &g,
                        &key,
                        &outbe_tee_enclave::dev::CREDENTIAL_SECRET,
                    ),
                }
            })
        })
    }
}
