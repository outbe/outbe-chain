//! Domain-neutral direct dispatch of resident infrastructure requests.
use crate::{
    in_process::{InProcessError, InProcessTeeContext},
    protocol::{EnclaveRequest, EnclaveResponse},
    CallMeta, TransportError,
};
use std::sync::{Arc, RwLock};

mod verification;
pub use verification::{
    prepare_no_attestation_dev_onboarding_artifact_v1, verify_dcap_evidence_v1,
    verify_dcap_registration_and_seal_v1,
};

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
    #[error("local enclave permanent offer key is not ready; no recovery or fallback exists")]
    OfferKeyNotReady(#[source] InProcessError),
    #[error("unsupported in-process infrastructure request")]
    UnsupportedRequest,
    #[error("domain dispatch failed: {0}")]
    Domain(#[source] Box<dyn std::error::Error + Send + Sync>),
}

#[derive(Clone)]
struct Runtime {
    context: Arc<InProcessTeeContext>,
}
static RUNTIME: RwLock<Option<Runtime>> = RwLock::new(None);

/// Install one resident identity for infrastructure and typed domain calls.
pub fn install_in_process_tee_context(
    context: Arc<InProcessTeeContext>,
) -> Result<(), InstallError> {
    let mut runtime = RUNTIME
        .write()
        .map_err(|_| InstallError::Probe(InProcessDispatchError::LockPoisoned.into()))?;
    if runtime.is_some() {
        return Err(InstallError::AlreadyInitialized);
    }
    *runtime = Some(Runtime { context });
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
    Some(
        request_on(&runtime, meta, request)
            .map(|response| (runtime.context.attestation_public(), response)),
    )
}

fn execute_request(
    runtime: &Runtime,
    request: &EnclaveRequest,
) -> Result<EnclaveResponse, TransportError> {
    let context = &runtime.context;
    match request {
        EnclaveRequest::GetPublicKeys => {
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
        }
        _ => Err(InProcessDispatchError::UnsupportedRequest.into()),
    }
}

/// Dispatch `request` on one captured runtime. A later global swap is ignored.
fn dispatch_resident_request(
    runtime: &Runtime,
    request: &EnclaveRequest,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    let response = execute_request(runtime, request)?;
    Ok((runtime.context.attestation_public(), response))
}

/// One session snapshot: duration and the response share the captured `runtime`.
pub(crate) fn dispatch_session_snapshot(
    meta: CallMeta,
    request: &EnclaveRequest,
) -> Option<Result<([u8; 32], EnclaveResponse), TransportError>> {
    let runtime = runtime()?;
    let _in_flight = crate::metrics::ExecutionInFlightGuard::new();
    crate::metrics::record_tee_execution_lock_wait_ms(std::time::Duration::ZERO);
    let started = std::time::Instant::now();
    let result = dispatch_resident_request(&runtime, request);
    crate::metrics::record_request_duration(meta.label, started.elapsed());
    if let Err(error) = &result {
        crate::metrics::record_request_error(meta.label, error.metric_class());
    }
    Some(result)
}

fn request_on(
    runtime: &Runtime,
    meta: CallMeta,
    request: &EnclaveRequest,
) -> Result<EnclaveResponse, TransportError> {
    let _in_flight = crate::metrics::ExecutionInFlightGuard::new();
    // Direct execution has no execution mutex, so its lock-wait sample is exactly zero.
    crate::metrics::record_tee_execution_lock_wait_ms(std::time::Duration::ZERO);
    let started = std::time::Instant::now();
    let result = execute_request(runtime, request);
    crate::metrics::record_request_duration(meta.label, started.elapsed());
    if let Err(error) = &result {
        crate::metrics::record_request_error(meta.label, error.metric_class());
    }
    result
}

/// Whether a resident runtime is installed on this execution path.
pub fn is_enclave_configured() -> bool {
    in_process_tee_context().is_some()
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
    resident_network_encryption_public_key_state_v1()?
        .ok_or_else(|| InProcessDispatchError::OfferKeyNotReady(InProcessError::NotReady).into())
}

/// Run one non-consensus typed closure against the installed resident context.
///
/// Returns `None` only when no resident context is installed. Neither argument
/// nor result bytes are serialized.
pub(crate) fn try_session_typed<T>(
    meta: CallMeta,
    execute: impl FnOnce(&InProcessTeeContext) -> Result<T, TransportError>,
) -> Option<Result<T, TransportError>> {
    let context = in_process_tee_context()?;
    let started = std::time::Instant::now();
    let result = execute(&context);
    crate::metrics::record_request_duration(meta.label, started.elapsed());
    if let Err(error) = &result {
        crate::metrics::record_request_error(meta.label, error.metric_class());
    }
    Some(result)
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
    context: Option<Arc<InProcessTeeContext>>,
) -> InProcessTestGuard {
    let runtime = context.map(|context| Runtime { context });
    let previous = TEST_RUNTIME.with(|slot| slot.replace(Some(runtime)));
    InProcessTestGuard {
        previous,
        _not_send: std::marker::PhantomData,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::resident_context;
    use alloy_primitives::{B256, U256};
    use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
    use std::error::Error;
    use std::sync::Arc;

    #[test]
    fn unonboarded_offer_key_is_typed_and_node_local() {
        let binding = NetworkBindingV1 {
            chain_id: U256::from(676).to_be_bytes(),
            genesis_hash: B256::repeat_byte(2),
            attestation_mode: AttestationMode::NoAttestationDev,
        };
        let context = Arc::new(resident_context(7, binding));
        let _guard = set_in_process_tee_context_for_test(Some(context));
        let error = resident_network_encryption_public_key_v1().unwrap_err();
        assert!(error.is_node_local());
        let TransportError::InProcess(InProcessDispatchError::OfferKeyNotReady(source)) = &error
        else {
            panic!("{error}");
        };
        assert!(matches!(source, InProcessError::NotReady));
        let cause = Error::source(&error)
            .and_then(Error::source)
            .and_then(|inner| inner.downcast_ref::<InProcessError>());
        assert!(matches!(cause, Some(InProcessError::NotReady)));
    }

    thread_local! {
        static NEXT_CONTEXT: std::cell::RefCell<Option<Arc<InProcessTeeContext>>> =
            const { std::cell::RefCell::new(None) };
        static HELD_GUARD: std::cell::RefCell<Option<InProcessTestGuard>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Drops the successor guard before the outer install guard restores the thread.
    struct ReleaseSnapshotSwap;
    impl Drop for ReleaseSnapshotSwap {
        fn drop(&mut self) {
            HELD_GUARD.with(|slot| drop(slot.borrow_mut().take()));
            NEXT_CONTEXT.with(|slot| drop(slot.borrow_mut().take()));
        }
    }

    #[test]
    fn tee_session_call_uses_one_runtime_snapshot_across_a_global_swap() {
        let binding = NetworkBindingV1 {
            chain_id: U256::from(676).to_be_bytes(),
            genesis_hash: B256::repeat_byte(2),
            attestation_mode: AttestationMode::NoAttestationDev,
        };
        let first = Arc::new(resident_context(4, binding));
        let second = Arc::new(resident_context(9, binding));
        assert_ne!(first.attestation_public(), second.attestation_public());
        NEXT_CONTEXT.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&second)));
        let _outer = set_in_process_tee_context_for_test(Some(Arc::clone(&first)));
        let _release = ReleaseSnapshotSwap;
        let seen = crate::tee_call::tee_session_typed_call(
            CallMeta::session_only("snapshot"),
            |context| {
                let seen = context.attestation_public();
                let next = NEXT_CONTEXT
                    .with(|slot| slot.borrow_mut().take())
                    .expect("successor context");
                let guard = set_in_process_tee_context_for_test(Some(next));
                HELD_GUARD.with(|slot| *slot.borrow_mut() = Some(guard));
                Ok(seen)
            },
        )
        .expect("runtime installed")
        .expect("session call");
        assert_eq!(seen, first.attestation_public());
        assert_eq!(
            in_process_tee_context()
                .expect("successor installed")
                .attestation_public(),
            second.attestation_public()
        );
    }
}
