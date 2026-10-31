//! Host-side enclave client for the confidential Fidelity cohort path.
//!
//! Every RCFI/league value and every cohort mutation routes through the enclave:
//! [`crate::runtime`] reads the current cohort ciphertext from committed storage,
//! hands it to the enclave, and stores the returned ciphertext verbatim. This module
//! mirrors [`outbe_gratis`]'s `enclave_client`. It has the same determinism
//! (canonical-hash recheck), attestation (verify-then-discard), and
//! `tee_sidecar_unavailable` failure mode.

use alloy_primitives::{Address, B256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::protocol::{
    fidelity_cohort_canonical_hash, fidelity_query_canonical_hash,
    fidelity_snapshot_canonical_hash, EnclaveRequest, EnclaveResponse, FidelityCohortRequest,
    FidelityCohortResult, FidelityLeagueEntry, FidelityQueryRequest, FidelityQueryResult,
    FidelitySnapshotRequest,
};
use outbe_tee::TransportError;

use crate::errors::FidelityFatalError;

/// Apply a standalone Fidelity cohort op inside the enclave and validate the
/// response (determinism recheck + attestation verify-then-discard).
pub(crate) fn apply_cohort_op(req: FidelityCohortRequest) -> Result<FidelityCohortResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_cohort(&req) {
        return result;
    }

    let expected_hash = fidelity_cohort_canonical_hash(&req);
    call_enclave(
        expected_hash,
        EnclaveRequest::ApplyFidelityCohortOp {
            request: Box::new(req),
        },
    )
}

/// Batch-snapshot per-owner leagues inside the enclave.
pub(crate) fn snapshot_leagues(req: FidelitySnapshotRequest) -> Result<Vec<FidelityLeagueEntry>> {
    let owners: Vec<Address> = req.entries.iter().map(|entry| entry.owner).collect();
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(leagues) = test_enclave::try_snapshot(&req) {
        return require_requested_owners(&owners, leagues?);
    }

    let expected_hash = fidelity_snapshot_canonical_hash(&req);
    let snapshot: LeagueSnapshot = call_consensus_enclave(
        expected_hash,
        EnclaveRequest::SnapshotFidelityLeagues {
            request: Box::new(req),
        },
    )?;
    require_requested_owners(&owners, snapshot.leagues)
}

/// Rejects a snapshot that is not one league per requested owner, in request order.
pub(crate) fn require_requested_owners(
    owners: &[Address],
    leagues: Vec<FidelityLeagueEntry>,
) -> Result<Vec<FidelityLeagueEntry>> {
    let bound = leagues.len() == owners.len()
        && leagues
            .iter()
            .zip(owners)
            .all(|(entry, owner)| entry.owner == *owner);
    if !bound {
        return Err(FidelityFatalError::SnapshotUnbound.into());
    }
    Ok(leagues)
}

/// Owner-authorized RCFI/league query inside the enclave (eth_call path).
pub(crate) fn query_index(req: FidelityQueryRequest) -> Result<FidelityQueryResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_query(&req) {
        return result;
    }

    let expected_hash = fidelity_query_canonical_hash(&req);
    call_query_enclave(
        expected_hash,
        EnclaveRequest::QueryFidelityIndex {
            request: Box::new(req),
        },
    )
}

fn call_enclave<T: AttestedAnswer>(expected_hash: B256, request: EnclaveRequest) -> Result<T> {
    let (attestation_pub, response) = outbe_tee::try_with_enclave(|client| {
        let attestation_pub = client.attestation_pub();
        let response = client.request(&request);
        (attestation_pub, response)
    })
    .ok_or(FidelityFatalError::SidecarUnavailable {
        context: T::OPERATION,
    })?;
    let response =
        response.map_err(|e| FidelityFatalError::sidecar_request_failed(T::OPERATION, e))?;
    validate_answer(expected_hash, &attestation_pub, response)
}

fn call_consensus_enclave<T: AttestedAnswer>(
    expected_hash: B256,
    request: EnclaveRequest,
) -> Result<T> {
    let (attestation_pub, response) = outbe_tee::consensus_call(
        T::OPERATION,
        || {
            outbe_tee::try_with_enclave(|client| {
                let attestation_pub = client.attestation_pub();
                client
                    .request(&request)
                    .map(|response| (attestation_pub, response))
            })
            .ok_or_else(|| TransportError::Unavailable("enclave session unavailable".into()))?
        },
        |key, response| authenticate_answer::<T>(expected_hash, key, response),
    )
    .map_err(|error| map_consensus_error(T::OPERATION, error))?;
    validate_answer(expected_hash, &attestation_pub, response)
}

fn authenticate_answer<T: AttestedAnswer>(
    expected_hash: B256,
    attestation_pub: &[u8; 32],
    response: &EnclaveResponse,
) -> core::result::Result<(), TransportError> {
    let answer =
        T::from_response(response.clone()).map_err(|_| TransportError::UnexpectedResponse)?;
    if answer.inputs_canonical_hash() != expected_hash {
        return Err(TransportError::UnexpectedResponse);
    }
    answer.verify_attestation(attestation_pub)
}

fn call_query_enclave(expected_hash: B256, request: EnclaveRequest) -> Result<FidelityQueryResult> {
    let (attestation_pub, response) = outbe_tee::consensus_call(
        "QueryFidelityIndex",
        || {
            outbe_tee::try_with_enclave(|client| {
                let attestation_pub = client.attestation_pub();
                client
                    .request(&request)
                    .map(|response| (attestation_pub, response))
            })
            .ok_or_else(|| TransportError::Unavailable("enclave session unavailable".into()))?
        },
        |key, response| match response {
            EnclaveResponse::FidelityIndexQueried { result } => {
                if result.inputs_canonical_hash != expected_hash {
                    return Err(TransportError::UnexpectedResponse);
                }
                outbe_tee::verify_fidelity_query_attestation(
                    key,
                    result.inputs_canonical_hash,
                    result,
                    &result.attestation_tag,
                )
            }
            EnclaveResponse::FidelityQueryRejected {
                reason,
                inputs_canonical_hash,
                attestation_tag,
            } => verify_query_rejection(
                key,
                expected_hash,
                *inputs_canonical_hash,
                reason,
                attestation_tag,
            ),
            _ => Err(TransportError::UnexpectedResponse),
        },
    )
    .map_err(|error| map_consensus_error("QueryFidelityIndex", error))?;

    match response {
        EnclaveResponse::FidelityIndexQueried { .. } => {
            validate_answer(expected_hash, &attestation_pub, response)
        }
        EnclaveResponse::FidelityQueryRejected {
            reason,
            inputs_canonical_hash,
            attestation_tag,
        } => {
            verify_query_rejection(
                &attestation_pub,
                expected_hash,
                inputs_canonical_hash,
                &reason,
                &attestation_tag,
            )
            .map_err(FidelityFatalError::attestation_invalid)?;
            Err(crate::errors::FidelityError::QueryFailed { reason }.into())
        }
        other => Err(failed_response("QueryFidelityIndex", other)),
    }
}

fn verify_query_rejection(
    key: &[u8; 32],
    expected_hash: B256,
    inputs_hash: B256,
    reason: &str,
    tag: &[u8],
) -> core::result::Result<(), TransportError> {
    if inputs_hash != expected_hash {
        return Err(TransportError::UnexpectedResponse);
    }
    let preimage = outbe_tee::response_auth::attestation(
        b"outbe/fidelity/query-rejected/v1",
        expected_hash,
        reason,
    )
    .map_err(TransportError::codec)?;
    outbe_tee::tribute_v2::verify_attestation(key, &preimage, tag)
}

fn map_consensus_error(operation: &'static str, error: TransportError) -> FidelityFatalError {
    match error {
        TransportError::EnclaveError(message) => FidelityFatalError::EnclaveOpError {
            context: operation,
            message,
        },
        other => FidelityFatalError::sidecar_request_failed(operation, other),
    }
}

#[cfg(test)]
mod consensus_tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn signed_query_rejection_requires_its_expected_hash_and_reason() {
        let signer = SigningKey::from_bytes(&[0x71; 32]);
        let expected = B256::repeat_byte(0x42);
        let reason = "query is not available";
        let preimage = outbe_tee::response_auth::attestation(
            b"outbe/fidelity/query-rejected/v1",
            expected,
            reason,
        )
        .unwrap();
        let tag = signer.sign(&preimage).to_bytes();

        assert!(verify_query_rejection(
            signer.verifying_key().as_bytes(),
            expected,
            expected,
            reason,
            &tag,
        )
        .is_ok());
        assert!(verify_query_rejection(
            signer.verifying_key().as_bytes(),
            B256::ZERO,
            expected,
            reason,
            &tag,
        )
        .is_err());
        assert!(verify_query_rejection(
            signer.verifying_key().as_bytes(),
            expected,
            expected,
            "different reason",
            &tag,
        )
        .is_err());
    }
}

/// Checks the answer variant, then the canonical input hash, then the attestation tag.
pub(crate) fn validate_answer<T: AttestedAnswer>(
    expected_hash: B256,
    attestation_pub: &[u8; 32],
    response: EnclaveResponse,
) -> Result<T> {
    let answer = T::from_response(response)?;
    if answer.inputs_canonical_hash() != expected_hash {
        return Err(FidelityFatalError::EnclaveNondeterminism.into());
    }
    answer
        .verify_attestation(attestation_pub)
        .map_err(FidelityFatalError::attestation_invalid)?;
    Ok(answer)
}

fn failed_response(operation: &'static str, response: EnclaveResponse) -> PrecompileError {
    match response {
        EnclaveResponse::Error { message } => FidelityFatalError::EnclaveOpError {
            context: operation,
            message,
        }
        .into(),
        other => FidelityFatalError::UnexpectedEnclaveResponse(Box::new(other)).into(),
    }
}

/// A Fidelity enclave answer that carries its canonical input hash and attestation tag.
pub(crate) trait AttestedAnswer: Sized {
    const OPERATION: &'static str;

    fn from_response(response: EnclaveResponse) -> Result<Self>;

    fn inputs_canonical_hash(&self) -> B256;

    fn verify_attestation(
        &self,
        attestation_pub: &[u8; 32],
    ) -> core::result::Result<(), TransportError>;
}

macro_rules! boxed_answer {
    ($answer:ty, $variant:ident, $operation:literal, $verify:path) => {
        impl AttestedAnswer for $answer {
            const OPERATION: &'static str = $operation;

            fn from_response(response: EnclaveResponse) -> Result<Self> {
                match response {
                    EnclaveResponse::$variant { result } => Ok(*result),
                    other => Err(failed_response($operation, other)),
                }
            }

            fn inputs_canonical_hash(&self) -> B256 {
                self.inputs_canonical_hash
            }

            fn verify_attestation(
                &self,
                attestation_pub: &[u8; 32],
            ) -> core::result::Result<(), TransportError> {
                $verify(
                    attestation_pub,
                    self.inputs_canonical_hash,
                    self,
                    &self.attestation_tag,
                )
            }
        }
    };
}

boxed_answer!(
    FidelityCohortResult,
    FidelityCohortApplied,
    "ApplyFidelityCohortOp",
    outbe_tee::verify_fidelity_cohort_attestation
);
boxed_answer!(
    FidelityQueryResult,
    FidelityIndexQueried,
    "QueryFidelityIndex",
    outbe_tee::verify_fidelity_query_attestation
);

#[derive(Debug)]
pub(crate) struct LeagueSnapshot {
    pub(crate) leagues: Vec<FidelityLeagueEntry>,
    inputs_canonical_hash: B256,
    attestation_tag: Vec<u8>,
}

impl AttestedAnswer for LeagueSnapshot {
    const OPERATION: &'static str = "SnapshotFidelityLeagues";

    fn from_response(response: EnclaveResponse) -> Result<Self> {
        match response {
            EnclaveResponse::FidelityLeaguesSnapshotted {
                leagues,
                inputs_canonical_hash,
                attestation_tag,
            } => Ok(Self {
                leagues,
                inputs_canonical_hash,
                attestation_tag,
            }),
            other => Err(failed_response(Self::OPERATION, other)),
        }
    }

    fn inputs_canonical_hash(&self) -> B256 {
        self.inputs_canonical_hash
    }

    fn verify_attestation(
        &self,
        attestation_pub: &[u8; 32],
    ) -> core::result::Result<(), TransportError> {
        outbe_tee::verify_fidelity_snapshot_attestation(
            attestation_pub,
            self.inputs_canonical_hash,
            &self.leagues,
            &self.attestation_tag,
        )
    }
}

/// In-process enclave stand-in for tests (this crate's tests and any downstream
/// crate that enables the `test-enclave` feature). Runs the **real**
/// `outbe_tee_enclave::fidelity` engine against a fixed dev state key, so tests
/// exercise the full confidential path without an SGX sidecar. This path does not
/// check attestation (only the mock-enclave e2e verifies it).
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use super::*;
    use crate::errors::FidelityError;
    use alloy_primitives::B256;
    use std::cell::RefCell;

    thread_local! {
        static STATE_KEY: RefCell<Option<[u8; 32]>> = const { RefCell::new(None) };
    }

    const DEV_EPOCH: u64 = 0;

    /// Chain id the in-process enclave binds. Query auth is chain-scoped, so the
    /// resident chain must equal what the runtime derives from
    /// `storage.chain_id()`. Tests that exercise the query path build their
    /// storage with this id and sign over [`dev_chain`]. Cohort/snapshot ops do
    /// not verify chain id, so downstream crates may use any storage chain id.
    ///
    /// The group sig + chain are the SHARED dev fidelity identity
    /// ([`outbe_tee_enclave::dev`]). The gratis stand-in derives the same
    /// fidelity key when it applies a folded cohort section. Thus a folded mint
    /// and a standalone snapshot agree.
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
        STATE_KEY.with(|k| *k.borrow_mut() = Some(*key));
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

    pub(super) fn try_cohort(req: &FidelityCohortRequest) -> Option<Result<FidelityCohortResult>> {
        STATE_KEY.with(|k| {
            k.borrow().map(|key| {
                outbe_tee_enclave::fidelity::apply_cohort_op(&key, req).map_err(|e| {
                    FidelityFatalError::enclave_op_error("ApplyFidelityCohortOp", e).into()
                })
            })
        })
    }

    pub(super) fn try_snapshot(
        req: &FidelitySnapshotRequest,
    ) -> Option<Result<Vec<FidelityLeagueEntry>>> {
        STATE_KEY.with(|k| {
            k.borrow().map(|key| {
                outbe_tee_enclave::fidelity::snapshot_leagues(&key, req).map_err(|e| {
                    FidelityFatalError::enclave_op_error("SnapshotFidelityLeagues", e).into()
                })
            })
        })
    }

    pub(super) fn try_query(req: &FidelityQueryRequest) -> Option<Result<FidelityQueryResult>> {
        STATE_KEY.with(|k| {
            k.borrow().map(|key| {
                outbe_tee_enclave::fidelity::query_index(&key, dev_chain(), req)
                    .map_err(|e| FidelityError::enclave_query_error(e).into())
            })
        })
    }
}
