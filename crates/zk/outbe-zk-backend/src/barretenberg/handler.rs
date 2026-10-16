//! One handler owns the native API. The dispatcher and CRS share process-global state.
//! A mutex serializes complete operations on the calling thread.
//! Do not bypass this module with another FFI backend.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

use barretenberg_rs::api::BarretenbergApi;
use barretenberg_rs::backends::FfiBackend;
use barretenberg_rs::generated_types::{CircuitInfoResponse, CircuitInput, ProofSystemSettings};
use outbe_zk::error::Error;

use super::{configure_memory, srs, Barretenberg, CANONICAL_SRS_POINTS};
use crate::errors::{BackendProofError, CrsInitError, HandlerError};

#[cfg(test)]
mod tests;

static HANDLER: OnceLock<Result<BarretenbergHandler, HandlerError>> = OnceLock::new();

/// A process-wide interface to one native API.
///
/// Methods execute on the calling thread. A mutex serializes complete native operations.
/// There is no worker thread or request queue.
/// Async callers should use their runtime's facility for blocking tasks, including during initialization.
/// CRS initialization can read files or download data with a blocking HTTP client.
///
/// A mutable API borrow protects only one Rust object.
/// The singleton prevents independent objects from accessing the native dispatcher's global state concurrently.
///
/// Set the CRS path before initialization. Set the CRS capacity before initialization.
/// Set `HARDWARE_CONCURRENCY` before native calls, for example `HARDWARE_CONCURRENCY=8`.
/// Use a positive integer within the limits that the native implementation supports.
/// Do not change this environment variable while native operations execute.
///
/// The pinned native implementation caches concurrency for each calling thread.
/// Its default is `min(32, detected concurrency)`. Builds with `NO_MULTITHREADING` use one CPU.
/// More threads may improve proving performance or decrease performance.
/// Measure verification throughput to determine the effect.
/// This setting does not permit concurrent dispatcher calls or parallel execution of Rust Poseidon2.
///
/// Restart the process after initialization failure or a panic during a native operation.
/// Another reference to this interface does not create an independent engine or reset native state.
pub struct BarretenbergHandler {
    api: Mutex<BarretenbergApi<FfiBackend>>,
}

impl BarretenbergHandler {
    /// Initialize the canonical capacity if necessary. Wait until initialization completes.
    pub fn global() -> Result<&'static Self, HandlerError> {
        Self::initialize(CANONICAL_SRS_POINTS)
    }

    /// Return an initialized handler. Do not load or download CRS data.
    pub fn initialized() -> Result<&'static Self, HandlerError> {
        HANDLER
            .get()
            .ok_or(HandlerError::NotInitialized)?
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(super) fn initialize(points: u32) -> Result<&'static Self, HandlerError> {
        HANDLER
            .get_or_init(|| {
                catch_unwind(AssertUnwindSafe(|| {
                    if let Ok(path) = std::env::var("OUTBE_BB_SRS_PATH") {
                        srs::set_srs_path(path.into());
                    }
                    let backend = FfiBackend::new().map_err(|error| {
                        HandlerError::Initialization(CrsInitError::Failed(error.to_string()))
                    })?;
                    let mut api = BarretenbergApi::new(backend);
                    srs::ensure_srs(&mut api, points)
                        .map_err(|error| HandlerError::Initialization(error.into()))?;
                    Ok(Self {
                        api: Mutex::new(api),
                    })
                }))
                .unwrap_or(Err(HandlerError::Initialization(CrsInitError::Panicked)))
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Check the fixed native CRS capacity. This method cannot increase the capacity.
    pub fn ensure_capacity(&self, points: u32) -> Result<(), HandlerError> {
        execute(&self.api, |api| srs::ensure_srs(api, points))
    }

    /// Calculate native circuit statistics on the calling thread.
    /// This method blocks. It requires successful CRS initialization, including for diagnostic calls.
    pub fn circuit_stats(
        &self,
        circuit: CircuitInput,
        include_gates: bool,
        settings: ProofSystemSettings,
    ) -> Result<CircuitInfoResponse, HandlerError> {
        execute(&self.api, |api| {
            Ok(api
                .circuit_stats(circuit, include_gates, settings)
                .map_err(|error| BackendProofError::BbCircuitStats(error.to_string()))?)
        })
    }

    pub(super) fn prove(
        &self,
        circuit: CircuitInput,
        witness: Vec<u8>,
        options: Barretenberg,
    ) -> Result<Vec<Vec<u8>>, HandlerError> {
        execute(&self.api, |api| {
            configure_memory(options.low_memory, options.max_storage_usage);
            let settings = super::settings_ultra_honk_keccak(options.disable_zk);
            srs::ensure_srs_for(api, &circuit.bytecode, &settings)?;
            let result = api
                .circuit_prove(circuit, &witness, settings)
                .map_err(|error| BackendProofError::BbProve(error.to_string()))?;
            Ok(result.proof)
        })
    }

    pub(super) fn verify(
        &self,
        vk: Vec<u8>,
        public: Vec<Vec<u8>>,
        proof: Vec<Vec<u8>>,
        acir: Option<Vec<u8>>,
        settings: ProofSystemSettings,
    ) -> Result<bool, HandlerError> {
        execute(&self.api, |api| {
            if let Some(acir) = &acir {
                srs::ensure_srs_for(api, acir, &settings)?;
            }
            let result = api
                .circuit_verify(&vk, public, proof, settings)
                .map_err(|error| {
                    if acir.is_some() {
                        BackendProofError::BbVerify(error.to_string())
                    } else {
                        BackendProofError::BbRawVerify(error.to_string())
                    }
                })?;
            Ok(result.verified)
        })
    }
}

fn execute<T, R>(
    state: &Mutex<T>,
    operation: impl FnOnce(&mut T) -> Result<R, Error>,
) -> Result<R, HandlerError> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut state = state.lock().map_err(|_| HandlerError::Terminal)?;
        operation(&mut state).map_err(|error| match error {
            // Do not repeat the generic proof error's display prefix during conversion.
            Error::Proof(message) => HandlerError::Command(message),
            other => HandlerError::Command(other.to_string()),
        })
    }))
    .unwrap_or(Err(HandlerError::Terminal))
}
