//! Domain-neutral direct dispatch. No lane lock is held across a payload callback.
use crate::{
    in_process::{InProcessError, InProcessTeeContext},
    protocol::{EnclaveRequest, EnclaveResponse, PayloadLane},
    CallMeta, InstallError, TransportError,
};
use std::sync::{Arc, OnceLock};

/// Domain adapter supplied by the engine owner, preserving typed failures.
pub type InProcessPayloadHandler =
    fn(&InProcessTeeContext, PayloadLane, &[u8]) -> Result<EnclaveResponse, TransportError>;

/// Structured failures at the in-process infrastructure/domain boundary.
#[derive(Debug, thiserror::Error)]
pub enum InProcessDispatchError {
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
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Install one identity and its domain callback atomically for all three lanes.
pub fn install_in_process_tee_context(
    context: Arc<InProcessTeeContext>,
    payload: InProcessPayloadHandler,
) -> Result<(), InstallError> {
    RUNTIME
        .set(Runtime { context, payload })
        .map_err(|_| InstallError::AlreadyInitialized)
}

fn runtime() -> Option<Runtime> {
    #[cfg(any(test, feature = "test-utils"))]
    if let Some(runtime) = TEST_RUNTIME.with(|slot| slot.borrow().clone()) {
        return runtime;
    }
    RUNTIME.get().cloned()
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
    let runtime = runtime()?;
    let lane = match request {
        EnclaveRequest::Payload { lane, .. } => *lane,
        _ => PayloadLane::Execution,
    };
    Some(request_on(&runtime, meta, request, lane))
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
        // Direct execution has no session mutex, so its lock-wait sample is exactly zero.
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

/// Reject incorrectly routed frames before invoking even a domain callback.
pub(crate) fn request_on_lane(
    meta: CallMeta,
    request: &EnclaveRequest,
    lane: PayloadLane,
) -> Option<Result<EnclaveResponse, TransportError>> {
    let runtime = runtime()?;
    Some(
        if matches!(request, EnclaveRequest::Payload {lane: actual, ..} if *actual == lane)
            || (lane == PayloadLane::Canary
                && matches!(
                    request,
                    EnclaveRequest::Health | EnclaveRequest::GetPublicKeys
                ))
        {
            request_on(&runtime, meta, request, lane)
        } else {
            Err(InProcessDispatchError::LaneDenied { lane }.into())
        },
    )
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
