//! One process-wide gate for the native bbapi dispatcher and its global state.
//!
//! Keep complete SRS/proof sessions inside one scope. A scoped callback must
//! not reacquire this gate, including through a public protocol hash function.

use barretenberg_rs::{
    api::BarretenbergApi, backend::Backend, backends::FfiBackend, error::BarretenbergError,
};
use std::sync::{Arc, Mutex};

/// API borrowed only while the shared gate is held.
pub type NativeApi = BarretenbergApi<NativeBackend>;

/// Preserve error provenance without changing the binding or native protocol.
/// The pinned FFI tags null/empty raw responses as `Backend`, the same variant
/// the API uses for a successfully decoded command ErrorResponse. Remap only
/// raw transport errors here, before the API decodes successful response bytes.
pub struct NativeBackend(FfiBackend);

impl Backend for NativeBackend {
    fn call(&mut self, input: &[u8]) -> Result<Vec<u8>, BarretenbergError> {
        self.0.call(input).map_err(transport_error)
    }

    fn destroy(&mut self) -> Result<(), BarretenbergError> {
        self.0.destroy().map_err(transport_error)
    }
}

fn transport_error(error: BarretenbergError) -> BarretenbergError {
    match error {
        BarretenbergError::Backend(message) => BarretenbergError::Connection(message),
        other => other,
    }
}

/// Node-local native infrastructure failure. Poison requires process restart.
#[derive(Clone, Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("Barretenberg dispatcher lock poisoned; process restart required")]
    LockPoisoned,
    #[error("Barretenberg initialization failed: {0}")]
    Initialization(#[source] Arc<BarretenbergError>),
    #[error("Barretenberg transport or response failed: {0}")]
    Backend(#[source] Arc<BarretenbergError>),
    #[error("Barretenberg setup failed: {0}")]
    Setup(String),
    #[error("injected Barretenberg infrastructure failure")]
    InjectedFailure,
}

impl RuntimeError {
    /// These failures describe local infrastructure, not invalid user inputs.
    pub const fn is_infrastructure(&self) -> bool {
        true
    }
}

// Native errors are opaque; only the same retained cause compares equal.
impl PartialEq for RuntimeError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::LockPoisoned, Self::LockPoisoned)
            | (Self::InjectedFailure, Self::InjectedFailure) => true,
            (Self::Initialization(a), Self::Initialization(b))
            | (Self::Backend(a), Self::Backend(b)) => Arc::ptr_eq(a, b),
            (Self::Setup(a), Self::Setup(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for RuntimeError {}

static GATE: Mutex<()> = Mutex::new(());

/// Acquire the gate, construct the API and run a complete native session.
/// Ordinary command errors release the gate; panics poison it permanently.
pub fn with_api<T, E: From<RuntimeError>>(
    operation: impl FnOnce(&mut NativeApi) -> Result<T, E>,
) -> Result<T, E> {
    scoped(&GATE, operation)
}

fn scoped<T, E: From<RuntimeError>>(
    gate: &Mutex<()>,
    operation: impl FnOnce(&mut NativeApi) -> Result<T, E>,
) -> Result<T, E> {
    let _guard = gate.lock().map_err(|_| RuntimeError::LockPoisoned)?;
    let backend =
        FfiBackend::new().map_err(|error| RuntimeError::Initialization(Arc::new(error)))?;
    operation(&mut BarretenbergApi::new(NativeBackend(backend)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_backend_error_is_transport_not_command_rejection() {
        let mapped = transport_error(BarretenbergError::Backend("arbitrary raw failure".into()));
        assert!(
            matches!(mapped, BarretenbergError::Connection(message) if message == "arbitrary raw failure")
        );
        assert!(matches!(
            transport_error(BarretenbergError::Deserialization("invalid bytes".into())),
            BarretenbergError::Deserialization(_)
        ));
    }

    #[test]
    fn poison_is_not_recovered() {
        let gate = Mutex::new(());
        let _ = std::panic::catch_unwind(|| {
            let _: Result<(), RuntimeError> =
                scoped(&gate, |_| panic!("poison isolated test gate"));
        });
        let result = scoped(&gate, |_| Ok::<_, RuntimeError>(()));
        assert!(matches!(result, Err(RuntimeError::LockPoisoned)));
        assert!(gate.is_poisoned());
    }
}
