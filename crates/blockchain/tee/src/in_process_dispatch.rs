//! Domain-neutral direct dispatch. No lane lock is held across a payload callback.
use crate::{
    in_process::{InProcessError, InProcessTeeContext},
    protocol::{EnclaveRequest, EnclaveResponse, PayloadLane},
    CallMeta, TransportError,
};
use std::sync::{Arc, RwLock};

mod verification;
pub use verification::{
    prepare_no_attestation_dev_onboarding_artifact_v1, verify_dcap_evidence_v1,
    verify_dcap_registration_and_seal_v1,
};

/// Domain adapter supplied by the engine owner, preserving typed failures.
pub type InProcessPayloadHandler =
    fn(&InProcessTeeContext, PayloadLane, &[u8]) -> Result<EnclaveResponse, TransportError>;

/// Install-time failure for the process-global resident runtime.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallError {
    #[error("resident TEE runtime already initialized")]
    AlreadyInitialized,
    #[error("enclave install probe failed: {0}")]
    Probe(#[from] TransportError),
}

/// Structured failures at the in-process infrastructure/domain boundary.
#[derive(Debug, thiserror::Error)]
pub enum InProcessDispatchError {
    #[error("in-process runtime lock poisoned")]
    LockPoisoned,
    #[error("resident TEE context is not installed")]
    ContextNotInstalled,
    #[error("active in-process context changed or is not installed")]
    ContextChanged,
    #[error("resident operation failed: {0}")]
    Resident(#[from] InProcessError),
    #[error("request is not permitted on lane {lane:?}")]
    LaneDenied { lane: PayloadLane },
    #[error("unsupported in-process infrastructure request")]
    UnsupportedRequest,
    #[error("domain dispatch failed: {0}")]
    Domain(#[source] Box<dyn std::error::Error + Send + Sync>),
}

#[derive(Clone)]
struct Runtime {
    context: Arc<InProcessTeeContext>,
    payload: InProcessPayloadHandler,
}
static RUNTIME: RwLock<Option<Runtime>> = RwLock::new(None);

/// Install one identity and its domain callback atomically for all three lanes.
pub fn install_in_process_tee_context(
    context: Arc<InProcessTeeContext>,
    payload: InProcessPayloadHandler,
) -> Result<(), InstallError> {
    let mut runtime = RUNTIME
        .write()
        .map_err(|_| InstallError::Probe(InProcessDispatchError::LockPoisoned.into()))?;
    if runtime.is_some() {
        return Err(InstallError::AlreadyInitialized);
    }
    *runtime = Some(Runtime { context, payload });
    Ok(())
}

fn runtime() -> Option<Runtime> {
    #[cfg(any(test, feature = "test-utils"))]
    if let Some(runtime) = TEST_RUNTIME.with(|slot| slot.borrow().clone()) {
        return runtime;
    }
    RUNTIME.read().ok()?.clone()
}

/// Commit promotion while preventing dispatch from observing a partial owner swap.
/// The callback must not re-enter dispatch and must validate before its first effect.
pub fn replace_in_process_tee_context<E: From<InProcessDispatchError>>(
    expected: &Arc<InProcessTeeContext>,
    candidate: Arc<InProcessTeeContext>,
    commit: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    fn replace<E: From<InProcessDispatchError>>(
        slot: &mut Option<Runtime>,
        expected: &Arc<InProcessTeeContext>,
        candidate: Arc<InProcessTeeContext>,
        commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        let runtime = slot
            .as_mut()
            .filter(|runtime| Arc::ptr_eq(&runtime.context, expected))
            .ok_or(InProcessDispatchError::ContextChanged)?;
        commit()?;
        runtime.context = candidate;
        Ok(())
    }
    #[cfg(any(test, feature = "test-utils"))]
    if TEST_RUNTIME.with(|slot| slot.borrow().is_some()) {
        return TEST_RUNTIME.with(|slot| {
            replace(
                slot.borrow_mut().as_mut().expect("test override"),
                expected,
                candidate,
                commit,
            )
        });
    }
    replace(
        &mut *RUNTIME
            .write()
            .map_err(|_| InProcessDispatchError::LockPoisoned)?,
        expected,
        candidate,
        commit,
    )
}

/// Read the installed RAM owner without acquiring an execution/session lock.
pub fn in_process_tee_context() -> Option<Arc<InProcessTeeContext>> {
    Some(runtime()?.context)
}

/// Dispatch an execution request without bypassing the caller's witness boundary.
pub fn try_in_process_request(
    meta: CallMeta,
    request: &EnclaveRequest,
) -> Option<Result<EnclaveResponse, TransportError>> {
    request_with_attestation(meta, request).map(|result| result.map(|(_, response)| response))
}

/// Bind the response to the exact resident context that dispatched it.
pub(crate) fn request_with_attestation(
    meta: CallMeta,
    request: &EnclaveRequest,
) -> Option<Result<([u8; 32], EnclaveResponse), TransportError>> {
    let runtime = runtime()?;
    let lane = match request {
        EnclaveRequest::Payload { lane, .. } => *lane,
        _ => PayloadLane::Execution,
    };
    Some(
        request_on(&runtime, meta, request, lane)
            .map(|response| (runtime.context.attestation_public(), response)),
    )
}

fn request_on(
    runtime: &Runtime,
    meta: CallMeta,
    request: &EnclaveRequest,
    lane: PayloadLane,
) -> Result<EnclaveResponse, TransportError> {
    let _in_flight =
        (lane == PayloadLane::Execution).then(crate::metrics::ExecutionInFlightGuard::new);
    if lane == PayloadLane::Execution {
        // Direct execution has no execution mutex, so its lock-wait sample is exactly zero.
        crate::metrics::record_tee_execution_lock_wait_ms(std::time::Duration::ZERO);
    }
    let started = std::time::Instant::now();
    let context = &runtime.context;
    let result = match request {
        EnclaveRequest::Payload { lane, payload } => (runtime.payload)(context, *lane, payload),
        EnclaveRequest::GetPublicKeys => (|| {
            let ready = context
                .offer_key_ready()
                .map_err(InProcessDispatchError::from)?;
            Ok(EnclaveResponse::PublicKeys {
                offer_key_ready: ready,
                recipient_x25519_pub: if ready {
                    context
                        .network_encryption_public()
                        .map_err(InProcessDispatchError::from)?
                } else {
                    context.recipient_public()
                },
                attestation_pub: context.attestation_public(),
                noise_static_pub: context.noise_public(),
                tee_bls_pub: context.tee_bls_public_bytes(),
                dkg_enc_pub: context.dkg_enc_public(),
                dkg_enc_sig: Vec::new(),
            })
        })(),
        _ => Err(InProcessDispatchError::UnsupportedRequest.into()),
    };
    crate::metrics::record_request_duration(meta.label, started.elapsed());
    if let Err(error) = &result {
        crate::metrics::record_request_error(meta.label, error.metric_class());
    }
    result
}

/// Reject incorrectly routed requests before invoking a domain callback.
fn request_on_lane(
    meta: CallMeta,
    request: &EnclaveRequest,
    lane: PayloadLane,
) -> Option<Result<EnclaveResponse, TransportError>> {
    let runtime = runtime()?;
    Some(
        if matches!(request, EnclaveRequest::Payload {lane: actual, ..} if *actual == lane)
            || (lane == PayloadLane::Canary && matches!(request, EnclaveRequest::GetPublicKeys))
        {
            request_on(&runtime, meta, request, lane)
        } else {
            Err(InProcessDispatchError::LaneDenied { lane }.into())
        },
    )
}

/// Whether a resident runtime is installed on this execution path.
pub fn is_enclave_configured() -> bool {
    in_process_tee_context().is_some()
}

/// Run an account-query request on its dedicated lane.
#[cfg(any(test, feature = "test-utils"))]
pub fn try_account_query(
    request: &EnclaveRequest,
) -> Option<Result<EnclaveResponse, TransportError>> {
    try_account_query_with_meta(CallMeta::session_only(request.label()), request)
}

/// Run an authorized account query with explicit call metadata.
pub fn try_account_query_with_meta(
    meta: CallMeta,
    request: &EnclaveRequest,
) -> Option<Result<EnclaveResponse, TransportError>> {
    request_on_lane(meta, request, PayloadLane::AccountQuery)
}

/// Run a canary probe with explicit call metadata.
#[cfg(any(test, feature = "test-utils"))]
pub fn canary_request_with_meta(
    meta: CallMeta,
    request: &EnclaveRequest,
) -> Result<EnclaveResponse, TransportError> {
    request_on_lane(meta, request, PayloadLane::Canary)
        .ok_or(InProcessDispatchError::ContextNotInstalled)?
}

/// Read the active resident attestation key without waiting for execution.
#[cfg(any(test, feature = "test-utils"))]
pub fn canary_attestation_pub() -> Option<[u8; 32]> {
    Some(in_process_tee_context()?.attestation_public())
}

/// Read the resident permanent network key; `None` means fresh/keyless.
pub fn resident_network_encryption_public_key_state_v1(
) -> Result<Option<alloy_primitives::B256>, TransportError> {
    let context = in_process_tee_context().ok_or(InProcessDispatchError::ContextNotInstalled)?;
    if context
        .offer_key_ready()
        .map_err(InProcessDispatchError::from)?
    {
        Ok(Some(
            context
                .network_encryption_public()
                .map_err(InProcessDispatchError::from)?
                .into(),
        ))
    } else {
        Ok(None)
    }
}

/// Require the permanent network key; a keyless identity is not a recovery path.
pub fn resident_network_encryption_public_key_v1() -> Result<alloy_primitives::B256, TransportError>
{
    resident_network_encryption_public_key_state_v1()?.ok_or_else(|| {
        TransportError::EnclaveError(
            "local enclave permanent offer key is not ready; no recovery or fallback exists".into(),
        )
    })
}

#[cfg(any(test, feature = "test-utils"))]
thread_local! {static TEST_RUNTIME: std::cell::RefCell<Option<Option<Runtime>>> = const {std::cell::RefCell::new(None)};}

/// Thread-local test replacement; restores the previous runtime on drop.
#[cfg(any(test, feature = "test-utils"))]
pub struct InProcessTestGuard {
    previous: Option<Option<Runtime>>,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}
#[cfg(any(test, feature = "test-utils"))]
impl Drop for InProcessTestGuard {
    fn drop(&mut self) {
        TEST_RUNTIME.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}
/// Override only this test thread, including explicitly disabling the installed runtime.
#[cfg(any(test, feature = "test-utils"))]
pub fn set_in_process_tee_context_for_test(
    runtime: Option<(Arc<InProcessTeeContext>, InProcessPayloadHandler)>,
) -> InProcessTestGuard {
    let runtime = runtime.map(|(context, payload)| Runtime { context, payload });
    let previous = TEST_RUNTIME.with(|slot| slot.replace(Some(runtime)));
    InProcessTestGuard {
        previous,
        _not_send: std::marker::PhantomData,
    }
}
