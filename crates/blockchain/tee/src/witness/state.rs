use super::typed_record::{proposer_record_typed, verifier_typed};
use super::{EnclaveWitnessEntryV1, EnclaveWitnessTapeV1};
use crate::{
    call_context::current_witness_state,
    errors::{WitnessAvailabilityError, WitnessCandidateRejected},
    protocol::EnclaveResponse,
    TransportError,
};
use std::{
    cell::Cell,
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessPolicy {
    EnclaveBacked,
    ReplayOnly,
}
static POLICY: OnceLock<WitnessPolicy> = OnceLock::new();
thread_local! { static TEST_POLICY: Cell<Option<WitnessPolicy>> = const { Cell::new(None) }; }

pub fn install_witness_policy(policy: WitnessPolicy) -> Result<(), WitnessPolicy> {
    POLICY.set(policy)
}
pub fn witness_policy() -> Option<WitnessPolicy> {
    TEST_POLICY
        .with(Cell::get)
        .or_else(|| POLICY.get().copied())
}
/// Thread-local override; production callers must use install_witness_policy.
#[cfg(any(test, feature = "test-utils"))]
pub fn set_witness_policy_for_test(policy: Option<WitnessPolicy>) {
    TEST_POLICY.with(|slot| slot.set(policy));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WitnessExecutionMode {
    ProposerRecord,
    VerifierWithEnclave,
    VerifierReplay,
    RpcDirect,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockWitnessFault {
    TapeInvalid,
    SemanticMismatch,
    LocalUnavailable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateRejection {
    UnsignedEnclaveError,
    BudgetDeferred,
}

/// Owned by one execution attempt. TLS only borrows its shared handle synchronously.
#[derive(Debug, Clone)]
pub struct WitnessState {
    pub mode: WitnessExecutionMode,
    pub leader_attestation_pub: Option<[u8; 32]>,
    pub tape: EnclaveWitnessTapeV1,
    pub prefix_cursor: usize,
    pub tx_cursor: usize,
    pub replay_cursor: usize,
    /// None denotes mandatory prefix work; Some is the zero-based body position.
    pub tx_index: Option<u32>,
    /// Set only for a pool user candidate, never prefix or reserved system work.
    pub is_user_candidate: bool,
    pub staged: Vec<EnclaveWitnessEntryV1>,
    pub first_witness_fault: Option<BlockWitnessFault>,
    pub candidate_rejection: Option<CandidateRejection>,
}
impl WitnessState {
    pub fn new(
        mode: WitnessExecutionMode,
        leader_attestation_pub: Option<[u8; 32]>,
        tape: EnclaveWitnessTapeV1,
    ) -> Self {
        Self {
            mode,
            leader_attestation_pub,
            tape,
            prefix_cursor: 0,
            tx_cursor: 0,
            replay_cursor: 0,
            tx_index: None,
            is_user_candidate: false,
            staged: Vec::new(),
            first_witness_fault: None,
            candidate_rejection: None,
        }
    }
    pub fn latch(&mut self, fault: BlockWitnessFault) {
        self.first_witness_fault.get_or_insert(fault);
    }
    fn next_entry(&mut self) -> Result<EnclaveWitnessEntryV1, TransportError> {
        let entry = if let Some(index) = self.tx_index {
            self.tape
                .txs
                .get(self.tx_cursor)
                .filter(|tx| tx.tx_index == index)
                .and_then(|tx| tx.entries.get(self.replay_cursor))
        } else {
            self.tape.prefix.get(self.prefix_cursor)
        };
        match entry.cloned() {
            Some(entry) => Ok(entry),
            None => {
                self.latch(BlockWitnessFault::TapeInvalid);
                Err(fault_error(BlockWitnessFault::TapeInvalid))
            }
        }
    }
    pub(super) fn consume_entry(&mut self) {
        if self.tx_index.is_some() {
            self.replay_cursor += 1;
        } else {
            self.prefix_cursor += 1;
        }
    }
}
pub(super) fn fault_error(fault: BlockWitnessFault) -> TransportError {
    let cause = match fault {
        BlockWitnessFault::TapeInvalid => WitnessAvailabilityError::TapeInvalid,
        BlockWitnessFault::SemanticMismatch => WitnessAvailabilityError::SemanticMismatch,
        BlockWitnessFault::LocalUnavailable => WitnessAvailabilityError::LocalUnavailable,
    };
    TransportError::unavailable(cause)
}
pub(super) fn locked(
    state: &Arc<Mutex<WitnessState>>,
) -> Result<std::sync::MutexGuard<'_, WitnessState>, TransportError> {
    state
        .lock()
        .map_err(|_| TransportError::unavailable(WitnessAvailabilityError::LockPoisoned))
}
fn unsigned_failure(state: &Arc<Mutex<WitnessState>>) -> Result<(), TransportError> {
    let mut state = locked(state)?;
    if state.mode == WitnessExecutionMode::ProposerRecord
        && state.tx_index.is_some()
        && state.is_user_candidate
    {
        state
            .candidate_rejection
            .get_or_insert(CandidateRejection::UnsignedEnclaveError);
    } else if state.mode == WitnessExecutionMode::VerifierWithEnclave {
        state.latch(BlockWitnessFault::LocalUnavailable);
    } else {
        state.latch(BlockWitnessFault::TapeInvalid);
    }
    Ok(())
}
fn local_answer(
    state: &Arc<Mutex<WitnessState>>,
    transport: impl FnOnce() -> Result<([u8; 32], EnclaveResponse), TransportError>,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(transport)) {
        Ok(outcome) => outcome,
        Err(_panic_payload) => {
            locked(state)?.latch(BlockWitnessFault::LocalUnavailable);
            return Err(TransportError::unavailable(
                WitnessAvailabilityError::TransportPanicked,
            ));
        }
    };
    match outcome {
        Ok((_, EnclaveResponse::Error { message })) => {
            unsigned_failure(state)?;
            Err(TransportError::enclave_error(
                crate::errors::EnclaveResponseError(message),
            ))
        }
        Err(error) => {
            if matches!(
                error,
                TransportError::EnclaveCause { .. }
                    | TransportError::InProcess(crate::InProcessDispatchError::Domain(_))
            ) {
                unsigned_failure(state)?;
            } else {
                locked(state)?.latch(BlockWitnessFault::LocalUnavailable);
            }
            Err(error)
        }
        answer => answer,
    }
}

pub(super) struct VerifierCallContext {
    pub(super) with_enclave: bool,
    pub(super) leader_key: [u8; 32],
    pub(super) entry: EnclaveWitnessEntryV1,
}

enum PreparedCall {
    RpcDirect,
    Proposer { leader_key: [u8; 32] },
    Verifier(Box<VerifierCallContext>),
}

/// Typed repeat. [`WitnessCandidateRejected`] stays available through [`std::error::Error::source`].
fn typed_candidate_rejection() -> TransportError {
    TransportError::enclave_error(WitnessCandidateRejected)
}

fn prepare_consensus_call(
    handle: &Arc<Mutex<WitnessState>>,
    rejection: fn() -> TransportError,
) -> Result<PreparedCall, TransportError> {
    let mut state = locked(handle)?;
    if let Some(fault) = state.first_witness_fault {
        return Err(fault_error(fault));
    }
    if state.candidate_rejection.is_some() {
        return Err(rejection());
    }
    if state.mode == WitnessExecutionMode::RpcDirect {
        return Ok(PreparedCall::RpcDirect);
    }
    let Some(leader_key) = state.leader_attestation_pub else {
        state.latch(BlockWitnessFault::TapeInvalid);
        return Err(fault_error(BlockWitnessFault::TapeInvalid));
    };
    if state.mode == WitnessExecutionMode::ProposerRecord {
        return Ok(PreparedCall::Proposer { leader_key });
    }
    let entry = state.next_entry()?;
    Ok(PreparedCall::Verifier(Box::new(VerifierCallContext {
        with_enclave: state.mode == WitnessExecutionMode::VerifierWithEnclave,
        leader_key,
        entry,
    })))
}

fn proposer_record_call(
    handle: &Arc<Mutex<WitnessState>>,
    leader_key: [u8; 32],
    transport: impl FnOnce() -> Result<([u8; 32], EnclaveResponse), TransportError>,
    authenticate: impl Fn(&[u8; 32], &EnclaveResponse) -> Result<(), TransportError>,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    let (key, response) = local_answer(handle, transport)?;
    if key != leader_key {
        locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
        return Err(fault_error(BlockWitnessFault::LocalUnavailable));
    }
    if let Err(error) = authenticate(&key, &response) {
        locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
        return Err(error);
    }
    let entry = EnclaveWitnessEntryV1::try_from(response.clone()).map_err(|_| {
        if let Ok(mut state) = locked(handle) {
            state.latch(BlockWitnessFault::TapeInvalid);
        }
        TransportError::UnexpectedResponse
    })?;
    locked(handle)?.staged.push(entry);
    Ok((key, response))
}

fn verifier_call(
    handle: &Arc<Mutex<WitnessState>>,
    ctx: VerifierCallContext,
    transport: impl FnOnce() -> Result<([u8; 32], EnclaveResponse), TransportError>,
    authenticate: impl Fn(&[u8; 32], &EnclaveResponse) -> Result<(), TransportError>,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    let response = EnclaveResponse::try_from(ctx.entry.clone()).map_err(|_| {
        if let Ok(mut state) = locked(handle) {
            state.latch(BlockWitnessFault::TapeInvalid);
        }
        TransportError::UnexpectedResponse
    })?;
    if let Err(error) = authenticate(&ctx.leader_key, &response) {
        locked(handle)?.latch(BlockWitnessFault::TapeInvalid);
        return Err(error);
    }
    if ctx.with_enclave {
        verify_local_enclave_match(handle, &ctx.entry, transport, &authenticate)?;
    }
    locked(handle)?.consume_entry();
    Ok((ctx.leader_key, response))
}

fn verify_local_enclave_match(
    handle: &Arc<Mutex<WitnessState>>,
    entry: &EnclaveWitnessEntryV1,
    transport: impl FnOnce() -> Result<([u8; 32], EnclaveResponse), TransportError>,
    authenticate: impl Fn(&[u8; 32], &EnclaveResponse) -> Result<(), TransportError>,
) -> Result<(), TransportError> {
    let (key, local) = local_answer(handle, transport)?;
    if let Err(error) = authenticate(&key, &local) {
        locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
        return Err(error);
    }
    let local = EnclaveWitnessEntryV1::try_from(local).map_err(|_| {
        if let Ok(mut state) = locked(handle) {
            state.latch(BlockWitnessFault::LocalUnavailable);
        }
        TransportError::UnexpectedResponse
    })?;
    if !entry.semantic_eq_ignoring_attestation_tag(&local) {
        locked(handle)?.latch(BlockWitnessFault::SemanticMismatch);
        return Err(fault_error(BlockWitnessFault::SemanticMismatch));
    }
    Ok(())
}

fn note_typed_transport_error(
    handle: &Arc<Mutex<WitnessState>>,
    error: &TransportError,
) -> Result<(), TransportError> {
    if matches!(
        error,
        TransportError::EnclaveCause { .. }
            | TransportError::InProcess(crate::InProcessDispatchError::Domain(_))
    ) {
        unsigned_failure(handle)
    } else {
        locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
        Ok(())
    }
}

pub(super) fn typed_execute<T>(
    handle: &Arc<Mutex<WitnessState>>,
    transport: impl FnOnce() -> Result<([u8; 32], T), TransportError>,
) -> Result<([u8; 32], T), TransportError> {
    let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(transport)) {
        Ok(outcome) => outcome,
        Err(_panic_payload) => {
            locked(handle)?.latch(BlockWitnessFault::LocalUnavailable);
            return Err(TransportError::unavailable(
                WitnessAvailabilityError::TransportPanicked,
            ));
        }
    };
    match outcome {
        Ok(answer) => Ok(answer),
        Err(error) => {
            note_typed_transport_error(handle, &error)?;
            Err(error)
        }
    }
}

pub use super::typed_record::{TypedWitnessPayload, TypedWitnessPayloadRef};

/// Typed consensus operation. Live execution calls `transport` in memory.
/// Witness bytes are produced only while recording or comparing a live answer.
/// `VerifierReplay` decodes the recorded [`EnclaveWitnessEntryV1::Payload`] and
/// does not call `transport`.
pub fn consensus_typed_call<T>(
    transport: impl FnOnce() -> Result<([u8; 32], T), TransportError>,
    encode: impl FnOnce(&T) -> Result<TypedWitnessPayload, TransportError>,
    decode_and_auth: impl Fn(&[u8; 32], TypedWitnessPayloadRef<'_>) -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    let Some(handle) = current_witness_state() else {
        if witness_policy().is_some() {
            return Err(TransportError::unavailable(
                WitnessAvailabilityError::MissingWitnessScope,
            ));
        }
        return Ok(transport()?.1);
    };
    match prepare_consensus_call(&handle, typed_candidate_rejection)? {
        PreparedCall::RpcDirect => {
            if witness_policy() == Some(WitnessPolicy::ReplayOnly) {
                return Err(TransportError::unavailable(
                    WitnessAvailabilityError::PrivateExecutionUnavailable,
                ));
            }
            Ok(transport()?.1)
        }
        PreparedCall::Proposer { leader_key } => {
            proposer_record_typed(&handle, leader_key, transport, encode, &decode_and_auth)
        }
        PreparedCall::Verifier(ctx) => {
            verifier_typed(&handle, *ctx, transport, encode, &decode_and_auth)
        }
    }
}

/// One logical, authenticated operation. Replay never evaluates transport.
/// The paired key permits existing downstream validators to reuse their preimages.
pub fn consensus_call(
    _op: &str,
    transport: impl FnOnce() -> Result<([u8; 32], EnclaveResponse), TransportError>,
    authenticate: impl Fn(&[u8; 32], &EnclaveResponse) -> Result<(), TransportError>,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    let Some(handle) = current_witness_state() else {
        if witness_policy().is_some() {
            return Err(TransportError::unavailable(
                WitnessAvailabilityError::MissingWitnessScope,
            ));
        }
        // Standalone CLI and existing in-process test clients have no block policy.
        return transport();
    };
    match prepare_consensus_call(&handle, typed_candidate_rejection)? {
        PreparedCall::RpcDirect => {
            if witness_policy() == Some(WitnessPolicy::ReplayOnly) {
                return Err(TransportError::unavailable(
                    WitnessAvailabilityError::PrivateExecutionUnavailable,
                ));
            }
            transport()
        }
        PreparedCall::Proposer { leader_key } => {
            proposer_record_call(&handle, leader_key, transport, authenticate)
        }
        PreparedCall::Verifier(ctx) => verifier_call(&handle, *ctx, transport, authenticate),
    }
}
