//! Host caller for confidential Fidelity.
//!
//! Cohort ops are session-only (non-recordable, tag 0). League snapshots and
//! index queries are witness-recorded. The in-process stand-in short-circuits
//! before either call when a test installs a state key.

use std::fmt::Write as _;

use alloy_primitives::{Address, B256};
use outbe_core_common::confidential_tee::{
    attestation, consensus_witness_call, derive_resident_state_key, session_witness_call,
    DualWitnessAnswer, FIDELITY,
};
use outbe_primitives::error::Result;
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::{CallMeta, TransportError};

use crate::attest::{
    verify_fidelity_cohort_attestation, verify_fidelity_query_attestation,
    verify_fidelity_snapshot_attestation,
};
use crate::engine;
use crate::errors::{display_string, EngineError, FidelityError, FidelityFatalError, OpText};
use crate::protocol::{
    fidelity_cohort_attestation_preimage, fidelity_cohort_canonical_hash,
    fidelity_query_attestation_preimage, fidelity_query_canonical_hash,
    fidelity_snapshot_attestation_preimage, fidelity_snapshot_canonical_hash,
    FidelityCohortRequest, FidelityCohortResult, FidelityLeagueEntry, FidelityQueryRequest,
    FidelityQueryResult, FidelitySnapshotRequest,
};
use crate::witness::{FidelityLeagueSnapshot, FidelityQueryRejection};

const COHORT_OP: &str = "ApplyFidelityCohortOp";
const SNAPSHOT_OP: &str = "SnapshotFidelityLeagues";
const QUERY_OP: &str = "QueryFidelityIndex";
const QUERY_REJECTED_DOMAIN: &[u8] = b"outbe/fidelity/query-rejected/v1";
const STATE_EPOCH: u64 = 0;

/// Apply a standalone cohort op. Session-only: the response is not on the witness tape.
pub fn apply_cohort_op(req: FidelityCohortRequest) -> Result<FidelityCohortResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_cohort(&req) {
        return result;
    }
    let expected = fidelity_cohort_canonical_hash(&req);
    let result = session_witness_call(CallMeta::session_only(COHORT_OP), move |context| {
        execute_cohort(context, expected, &req)
    })
    .ok_or(FidelityFatalError::SidecarUnavailable { context: COHORT_OP })?
    .map_err(|error| map_consensus_error(COHORT_OP, error))?;
    finish_cohort(expected, result)
}

/// Batch-snapshot per-owner leagues. Recorded under tag 4.
pub fn snapshot_leagues(req: FidelitySnapshotRequest) -> Result<Vec<FidelityLeagueEntry>> {
    let owners: Vec<Address> = req.entries.iter().map(|entry| entry.owner).collect();
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(leagues) = test_enclave::try_snapshot(&req) {
        return require_requested_owners(&owners, leagues?);
    }
    let expected = fidelity_snapshot_canonical_hash(&req);
    let snapshot = consensus_witness_call(
        CallMeta::recorded(SNAPSHOT_OP),
        move |context| execute_snapshot(context, expected, &req),
        move |key, snapshot| authenticate_snapshot(expected, key, snapshot),
    )
    .map_err(|error| map_consensus_error(SNAPSHOT_OP, error))?;
    let snapshot = finish_snapshot(expected, snapshot)?;
    require_requested_owners(&owners, snapshot.leagues)
}

/// Owner-authorized index query. Recorded under tag 5 or tag 15.
pub fn query_index(req: FidelityQueryRequest) -> Result<FidelityQueryResult> {
    #[cfg(any(test, feature = "test-enclave"))]
    if let Some(result) = test_enclave::try_query(&req) {
        return result;
    }
    let expected = fidelity_query_canonical_hash(&req);
    let answer = consensus_witness_call(
        CallMeta::recorded(QUERY_OP),
        move |context| execute_query(context, expected, &req),
        move |key, answer| authenticate_query(expected, key, answer),
    )
    .map_err(|error| map_consensus_error(QUERY_OP, error))?;
    match answer {
        DualWitnessAnswer::Applied(result) => finish_query(expected, result),
        DualWitnessAnswer::Rejected(rejection) => finish_rejection(expected, rejection),
    }
}

/// Rejects a snapshot that is not one league per requested owner, in request order.
pub fn require_requested_owners(
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

fn execute_cohort(
    context: &InProcessTeeContext,
    expected: B256,
    req: &FidelityCohortRequest,
) -> std::result::Result<FidelityCohortResult, TransportError> {
    let key = resident_key(context)?;
    let mut result = engine::apply_cohort_op(&key, req).map_err(TransportError::enclave_error)?;
    let preimage = fidelity_cohort_attestation_preimage(result.inputs_canonical_hash, &result)?;
    result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
    // Same snapshot that signed. A later global swap must not re-bind the key.
    authenticate_cohort(expected, &context.attestation_public(), &result)?;
    Ok(result)
}

fn execute_snapshot(
    context: &InProcessTeeContext,
    expected: B256,
    req: &FidelitySnapshotRequest,
) -> std::result::Result<FidelityLeagueSnapshot, TransportError> {
    let key = resident_key(context)?;
    let leagues = engine::snapshot_leagues(&key, req).map_err(TransportError::enclave_error)?;
    let inputs_canonical_hash = fidelity_snapshot_canonical_hash(req);
    let preimage = fidelity_snapshot_attestation_preimage(inputs_canonical_hash, &leagues)?;
    let snapshot = FidelityLeagueSnapshot {
        leagues,
        inputs_canonical_hash,
        attestation_tag: context.sign_domain_attestation(&preimage).to_vec(),
    };
    authenticate_snapshot(expected, &context.attestation_public(), &snapshot)?;
    Ok(snapshot)
}

fn execute_query(
    context: &InProcessTeeContext,
    expected: B256,
    req: &FidelityQueryRequest,
) -> std::result::Result<
    DualWitnessAnswer<FidelityQueryResult, FidelityQueryRejection>,
    TransportError,
> {
    let chain = B256::from(context.manifest().chain_id);
    let key = resident_key(context)?;
    let answer = match engine::query_index(&key, chain, req) {
        Ok(mut result) => {
            let preimage =
                fidelity_query_attestation_preimage(result.inputs_canonical_hash, &result)?;
            result.attestation_tag = context.sign_domain_attestation(&preimage).to_vec();
            DualWitnessAnswer::Applied(result)
        }
        Err(error) => DualWitnessAnswer::Rejected(sign_rejection(context, req, &error)?),
    };
    authenticate_query(expected, &context.attestation_public(), &answer)?;
    Ok(answer)
}

fn sign_rejection(
    context: &InProcessTeeContext,
    req: &FidelityQueryRequest,
    error: &EngineError,
) -> std::result::Result<FidelityQueryRejection, TransportError> {
    // The reason is signed consensus output. The engine error is not returned.
    let reason = display_string(error);
    let inputs_canonical_hash = fidelity_query_canonical_hash(req);
    let preimage = attestation(QUERY_REJECTED_DOMAIN, inputs_canonical_hash, &reason)?;
    Ok(FidelityQueryRejection {
        inputs_canonical_hash,
        reason,
        attestation_tag: context.sign_domain_attestation(&preimage).to_vec(),
    })
}

fn resident_key(
    context: &InProcessTeeContext,
) -> std::result::Result<outbe_tee::resident::StateKeyHandle, TransportError> {
    derive_resident_state_key(context, &FIDELITY, STATE_EPOCH)
}

fn authenticate_cohort(
    expected: B256,
    key: &[u8; 32],
    result: &FidelityCohortResult,
) -> std::result::Result<(), TransportError> {
    if result.inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    verify_fidelity_cohort_attestation(
        key,
        result.inputs_canonical_hash,
        result,
        &result.attestation_tag,
    )
}

fn authenticate_snapshot(
    expected: B256,
    key: &[u8; 32],
    snapshot: &FidelityLeagueSnapshot,
) -> std::result::Result<(), TransportError> {
    if snapshot.inputs_canonical_hash != expected {
        return Err(TransportError::UnexpectedResponse);
    }
    verify_fidelity_snapshot_attestation(
        key,
        snapshot.inputs_canonical_hash,
        &snapshot.leagues,
        &snapshot.attestation_tag,
    )
}

fn authenticate_query(
    expected: B256,
    key: &[u8; 32],
    answer: &DualWitnessAnswer<FidelityQueryResult, FidelityQueryRejection>,
) -> std::result::Result<(), TransportError> {
    match answer {
        DualWitnessAnswer::Applied(result) => {
            if result.inputs_canonical_hash != expected {
                return Err(TransportError::UnexpectedResponse);
            }
            verify_fidelity_query_attestation(
                key,
                result.inputs_canonical_hash,
                result,
                &result.attestation_tag,
            )
        }
        DualWitnessAnswer::Rejected(rejection) => authenticate_rejection(expected, key, rejection),
    }
}

fn authenticate_rejection(
    expected: B256,
    key: &[u8; 32],
    rejection: &FidelityQueryRejection,
) -> std::result::Result<(), TransportError> {
    outbe_tee::tee_call::verify_signed_rejection(
        key,
        outbe_tee::tee_call::SignedRejectionRef {
            domain: QUERY_REJECTED_DOMAIN,
            expected_inputs_hash: expected,
            actual_inputs_hash: rejection.inputs_canonical_hash,
            reason: &rejection.reason,
            tag: &rejection.attestation_tag,
        },
    )
    .map_err(|cause| match cause {
        outbe_tee::TeeAttestationError::InputsHashMismatch { .. } => {
            TransportError::UnexpectedResponse
        }
        other => TransportError::app_attestation(crate::attest::FIDELITY_ATTESTATION, other),
    })
}

fn finish_cohort(expected: B256, result: FidelityCohortResult) -> Result<FidelityCohortResult> {
    if result.inputs_canonical_hash != expected {
        return Err(FidelityFatalError::EnclaveNondeterminism.into());
    }
    Ok(result)
}

fn finish_snapshot(
    expected: B256,
    snapshot: FidelityLeagueSnapshot,
) -> Result<FidelityLeagueSnapshot> {
    if snapshot.inputs_canonical_hash != expected {
        return Err(FidelityFatalError::EnclaveNondeterminism.into());
    }
    Ok(snapshot)
}

fn finish_query(expected: B256, result: FidelityQueryResult) -> Result<FidelityQueryResult> {
    if result.inputs_canonical_hash != expected {
        return Err(FidelityFatalError::EnclaveNondeterminism.into());
    }
    Ok(result)
}

fn finish_rejection(
    expected: B256,
    rejection: FidelityQueryRejection,
) -> Result<FidelityQueryResult> {
    if rejection.inputs_canonical_hash != expected {
        return Err(FidelityFatalError::EnclaveNondeterminism.into());
    }
    Err(FidelityError::QueryFailed {
        reason: rejection.reason,
    }
    .into())
}

/// Map a witness or session transport error onto the historical fatal sentences.
pub fn map_consensus_error(operation: &'static str, error: TransportError) -> FidelityFatalError {
    match error {
        TransportError::EnclaveError(_) | TransportError::EnclaveCause { .. } => {
            FidelityFatalError::EnclaveOpError {
                context: operation,
                message: OpText(error),
            }
        }
        TransportError::AppAttestation { .. } => {
            FidelityFatalError::AttestationInvalid { source: error }
        }
        other => FidelityFatalError::SidecarRequestFailed {
            context: operation,
            source: other,
        },
    }
}

/// A typed answer for the wrong operation. There is no response enum to match.
pub fn unexpected_answer(operation: &'static str) -> FidelityFatalError {
    let mut detail = String::new();
    let _ = write!(
        &mut detail,
        "unexpected response from enclave for {operation}"
    );
    FidelityFatalError::SidecarRequestFailed {
        context: operation,
        source: TransportError::codec(UnexpectedAnswer(detail)),
    }
}

#[derive(Debug)]
struct UnexpectedAnswer(String);

impl std::fmt::Display for UnexpectedAnswer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for UnexpectedAnswer {}

#[cfg(test)]
mod consensus_tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn signed_query_rejection_requires_its_expected_hash_and_reason() {
        let signer = SigningKey::from_bytes(&[0x71; 32]);
        let expected = B256::repeat_byte(0x42);
        let reason = "query is not available";
        let preimage = attestation(QUERY_REJECTED_DOMAIN, expected, reason).unwrap();
        let tag = signer.sign(&preimage).to_bytes();
        let key = signer.verifying_key().to_bytes();
        let rejection = |hash: B256, text: &str| FidelityQueryRejection {
            inputs_canonical_hash: hash,
            reason: text.to_owned(),
            attestation_tag: tag.to_vec(),
        };
        assert!(authenticate_rejection(expected, &key, &rejection(expected, reason)).is_ok());
        assert!(authenticate_rejection(B256::ZERO, &key, &rejection(expected, reason)).is_err());
        assert!(
            authenticate_rejection(expected, &key, &rejection(expected, "different reason"))
                .is_err()
        );
    }

    const HASH: B256 = B256::repeat_byte(7);

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[9; 32])
    }

    fn sign_cohort(flip: bool) -> FidelityCohortResult {
        let mut result = FidelityCohortResult {
            outcome: crate::protocol::FidelityOpOutcome {
                new_blob: vec![1, 2, 3],
                qualified_start_initialized: Some(5),
                league: 3,
            },
            inputs_canonical_hash: HASH,
            attestation_tag: Vec::new(),
        };
        let preimage = fidelity_cohort_attestation_preimage(HASH, &result).expect("preimage");
        result.attestation_tag = signing_key().sign(&preimage).to_bytes().to_vec();
        result.attestation_tag[0] ^= u8::from(flip);
        result
    }

    fn sign_query(flip: bool) -> FidelityQueryResult {
        let mut result = FidelityQueryResult {
            rcfi: alloy_primitives::U256::from(11),
            efficiency: alloy_primitives::U256::from(12),
            league: 4,
            inputs_canonical_hash: HASH,
            attestation_tag: Vec::new(),
        };
        let preimage = fidelity_query_attestation_preimage(HASH, &result).expect("preimage");
        result.attestation_tag = signing_key().sign(&preimage).to_bytes().to_vec();
        result.attestation_tag[0] ^= u8::from(flip);
        result
    }

    fn sign_snapshot(flip: bool) -> FidelityLeagueSnapshot {
        let leagues = vec![crate::protocol::FidelityLeagueEntry {
            owner: alloy_primitives::Address::repeat_byte(1),
            league: 2,
            rcfi: alloy_primitives::U256::ZERO,
        }];
        let preimage = fidelity_snapshot_attestation_preimage(HASH, &leagues).expect("preimage");
        let mut attestation_tag = signing_key().sign(&preimage).to_bytes().to_vec();
        attestation_tag[0] ^= u8::from(flip);
        FidelityLeagueSnapshot {
            leagues,
            inputs_canonical_hash: HASH,
            attestation_tag,
        }
    }

    fn attestation_sentence(error: TransportError) -> String {
        match outbe_primitives::error::PrecompileError::from(map_consensus_error(COHORT_OP, error))
        {
            outbe_primitives::error::PrecompileError::EnclaveUnavailable(message) => message,
            other => panic!("expected EnclaveUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn authenticate_checks_the_hash_before_the_signature() {
        let key = signing_key().verifying_key().to_bytes();
        let other = B256::repeat_byte(8);
        assert!(authenticate_cohort(HASH, &key, &sign_cohort(false)).is_ok());
        assert!(matches!(
            authenticate_cohort(other, &key, &sign_cohort(false)),
            Err(TransportError::UnexpectedResponse)
        ));
        assert!(matches!(
            authenticate_cohort(other, &key, &sign_cohort(true)),
            Err(TransportError::UnexpectedResponse)
        ));
        let flipped = authenticate_cohort(HASH, &key, &sign_cohort(true)).unwrap_err();
        assert!(attestation_sentence(flipped).starts_with("tee_fidelity_attestation_invalid: "));

        assert!(authenticate_snapshot(HASH, &key, &sign_snapshot(false)).is_ok());
        assert!(matches!(
            authenticate_snapshot(other, &key, &sign_snapshot(true)),
            Err(TransportError::UnexpectedResponse)
        ));
        assert!(matches!(
            authenticate_snapshot(HASH, &key, &sign_snapshot(true)),
            Err(TransportError::AppAttestation { .. })
        ));

        let applied = DualWitnessAnswer::Applied(sign_query(false));
        assert!(authenticate_query(HASH, &key, &applied).is_ok());
        let flipped_query = DualWitnessAnswer::Applied(sign_query(true));
        assert!(matches!(
            authenticate_query(other, &key, &flipped_query),
            Err(TransportError::UnexpectedResponse)
        ));
        assert!(matches!(
            authenticate_query(HASH, &key, &flipped_query),
            Err(TransportError::AppAttestation { .. })
        ));
    }
}

#[cfg(test)]
mod executing_context {
    use super::*;
    use crate::protocol::{FidelityCohortOp, FidelityOpSection, FidelitySnapshotEntry};
    use alloy_primitives::{Address, U256};
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    use outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test;
    use outbe_tee::resident::ResidentOfferMaterial;
    use outbe_tee::witness::{
        EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope, WitnessState,
    };
    use std::sync::{Arc, Mutex};

    fn binding() -> NetworkBindingV1 {
        NetworkBindingV1 {
            chain_id: U256::from(676).to_be_bytes(),
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
            b"fidelity-witness-group-signature-v1!!!!".to_vec(),
            chain(),
            0,
            0,
        )
        .expect("offer material");
        context.activate_for_test(material).expect("activate offer");
        context
    }

    fn cohort_req() -> FidelityCohortRequest {
        FidelityCohortRequest {
            chain_id: chain(),
            account: Address::repeat_byte(0x11),
            amount: U256::ZERO,
            section: FidelityOpSection {
                op: FidelityCohortOp::Probe,
                timestamp: 1_000_000,
                first_qualified_start: 0,
                current_blob: Vec::new(),
            },
        }
    }

    fn snapshot_req() -> FidelitySnapshotRequest {
        FidelitySnapshotRequest {
            timestamp: 1_000_000,
            first_qualified_start: 0,
            entries: vec![FidelitySnapshotEntry {
                owner: Address::repeat_byte(0x11),
                cohort_blob: Vec::new(),
            }],
        }
    }

    fn query_req() -> FidelityQueryRequest {
        FidelityQueryRequest {
            chain_id: chain(),
            account: Address::repeat_byte(0x11),
            cohort_blob: Vec::new(),
            query_timestamp: 1_000_000,
            block_timestamp: 1_500_000,
            first_qualified_start: 0,
            expiry: 2_000_000,
            owner_sig: Vec::new(),
        }
    }

    #[test]
    fn rpc_direct_execute_rejects_a_foreign_hash_and_a_corrupt_tag() {
        let ctx = context(4);
        let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&ctx)));
        let state = Arc::new(Mutex::new(WitnessState::new(
            WitnessExecutionMode::RpcDirect,
            Some(ctx.attestation_public()),
            EnclaveWitnessTapeV1::default(),
        )));
        let _scope = WitnessExecutionScope::enter(state);
        let key = ctx.attestation_public();

        let cohort = cohort_req();
        let cohort_hash = fidelity_cohort_canonical_hash(&cohort);
        assert!(matches!(
            execute_cohort(&ctx, B256::ZERO, &cohort),
            Err(TransportError::UnexpectedResponse)
        ));
        let signed = execute_cohort(&ctx, cohort_hash, &cohort).unwrap();
        let mut corrupt = signed.clone();
        corrupt.attestation_tag[0] ^= 0xff;
        assert!(matches!(
            authenticate_cohort(cohort_hash, &key, &corrupt),
            Err(TransportError::AppAttestation { .. })
        ));

        let snapshot = snapshot_req();
        let snapshot_hash = fidelity_snapshot_canonical_hash(&snapshot);
        assert!(matches!(
            execute_snapshot(&ctx, B256::ZERO, &snapshot),
            Err(TransportError::UnexpectedResponse)
        ));
        let signed = execute_snapshot(&ctx, snapshot_hash, &snapshot).unwrap();
        let mut corrupt = signed.clone();
        corrupt.attestation_tag[0] ^= 0xff;
        assert!(matches!(
            authenticate_snapshot(snapshot_hash, &key, &corrupt),
            Err(TransportError::AppAttestation { .. })
        ));

        let query = query_req();
        let query_hash = fidelity_query_canonical_hash(&query);
        assert!(matches!(
            execute_query(&ctx, B256::ZERO, &query),
            Err(TransportError::UnexpectedResponse)
        ));
        let DualWitnessAnswer::Rejected(rejection) =
            execute_query(&ctx, query_hash, &query).unwrap()
        else {
            panic!("empty owner signature is a signed rejection");
        };
        let mut corrupt = rejection.clone();
        corrupt.attestation_tag[0] ^= 0xff;
        assert!(matches!(
            authenticate_rejection(query_hash, &key, &corrupt),
            Err(TransportError::AppAttestation { .. })
        ));
        corrupt.inputs_canonical_hash = B256::ZERO;
        assert!(matches!(
            authenticate_rejection(query_hash, &key, &corrupt),
            Err(TransportError::UnexpectedResponse)
        ));
    }

    #[test]
    fn finish_keeps_the_signature_after_the_global_context_is_replaced() {
        let first = context(4);
        let second = context(9);
        assert_ne!(first.attestation_public(), second.attestation_public());
        let _guard = set_in_process_tee_context_for_test(Some(Arc::clone(&first)));

        let cohort = cohort_req();
        let cohort_hash = fidelity_cohort_canonical_hash(&cohort);
        let cohort_result = execute_cohort(&first, cohort_hash, &cohort).unwrap();
        let snapshot = snapshot_req();
        let snapshot_hash = fidelity_snapshot_canonical_hash(&snapshot);
        let snapshot_result = execute_snapshot(&first, snapshot_hash, &snapshot).unwrap();
        let query = query_req();
        let query_hash = fidelity_query_canonical_hash(&query);
        let DualWitnessAnswer::Rejected(rejection) =
            execute_query(&first, query_hash, &query).unwrap()
        else {
            panic!("empty owner signature is a signed rejection");
        };

        let _successor = set_in_process_tee_context_for_test(Some(second.clone()));
        assert_eq!(
            outbe_tee::in_process_tee_context()
                .expect("successor")
                .attestation_public(),
            second.attestation_public()
        );
        assert!(
            authenticate_cohort(cohort_hash, &second.attestation_public(), &cohort_result).is_err()
        );
        assert!(finish_cohort(cohort_hash, cohort_result).is_ok());
        assert!(finish_snapshot(snapshot_hash, snapshot_result).is_ok());
        assert!(matches!(
            finish_rejection(query_hash, rejection).unwrap_err(),
            PrecompileError::RevertBytes(_)
        ));
    }
}

/// In-process stand-in. Runs the real engine against the shared dev state key
/// and does not check attestation. Production builds exclude this module.
#[cfg(any(test, feature = "test-enclave"))]
pub mod test_enclave {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static STATE_KEY: RefCell<Option<outbe_tee::resident::StateKeyHandle>> = const { RefCell::new(None) };
    }

    const DEV_EPOCH: u64 = 0;
    const GROUP_SIG: &[u8] = b"outbe-dev-fidelity-group-signature-fixed-seed";

    /// Chain id the stand-in binds. Query auth is chain-scoped.
    pub const DEV_CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;

    /// Resident chain id in the same encoding as `B256::from(U256::from(chain_id))`.
    pub fn dev_chain() -> B256 {
        B256::from(alloy_primitives::U256::from(DEV_CHAIN_ID))
    }

    fn fixture_key() -> outbe_tee::resident::StateKeyHandle {
        let material = outbe_tee::resident::ResidentOfferMaterial::for_test(
            GROUP_SIG.to_vec(),
            dev_chain(),
            DEV_EPOCH,
            0,
        )
        .expect("dev fidelity offer material");
        engine::derive_fidelity_state_key(&material, dev_chain(), DEV_EPOCH)
            .expect("derive dev fidelity state key")
    }

    /// Install the stand-in for this thread.
    pub fn install() {
        STATE_KEY.with(|key| *key.borrow_mut() = Some(fixture_key()));
    }

    /// Remove the stand-in for this thread.
    pub fn uninstall() {
        STATE_KEY.with(|key| *key.borrow_mut() = None);
    }

    /// Dev state handle, so a test can derive a view key and open a cohort blob.
    pub fn state_key() -> outbe_tee::resident::StateKeyHandle {
        assert!(
            STATE_KEY.with(|key| key.borrow().is_some()),
            "test enclave not installed"
        );
        fixture_key()
    }

    pub(super) fn try_cohort(req: &FidelityCohortRequest) -> Option<Result<FidelityCohortResult>> {
        with_key(|key| {
            engine::apply_cohort_op(key, req).map_err(|error| {
                map_consensus_error(COHORT_OP, TransportError::enclave_error(error)).into()
            })
        })
    }

    pub(super) fn try_snapshot(
        req: &FidelitySnapshotRequest,
    ) -> Option<Result<Vec<FidelityLeagueEntry>>> {
        with_key(|key| {
            engine::snapshot_leagues(key, req).map_err(|error| {
                map_consensus_error(SNAPSHOT_OP, TransportError::enclave_error(error)).into()
            })
        })
    }

    pub(super) fn try_query(req: &FidelityQueryRequest) -> Option<Result<FidelityQueryResult>> {
        with_key(|key| {
            engine::query_index(key, dev_chain(), req)
                .map_err(|source| FidelityError::EnclaveQueryError { source }.into())
        })
    }

    fn with_key<T>(
        body: impl FnOnce(&outbe_tee::resident::StateKeyHandle) -> Result<T>,
    ) -> Option<Result<T>> {
        STATE_KEY.with(|slot| slot.borrow().as_ref().map(body))
    }
}
