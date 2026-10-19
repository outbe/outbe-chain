//! One handler owns the native API. The dispatcher and CRS share process-global state.
//! A mutex serializes complete operations on the calling thread.
//! Do not bypass this module with another FFI backend.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, OnceLock};

use super::{configure_memory, srs, Barretenberg, CANONICAL_SRS_POINTS};
use crate::barretenberg::srs::load_srs;
use crate::errors::{BackendProofError, CrsError, HandlerError};
use barretenberg_rs::api::BarretenbergApi;
use barretenberg_rs::backends::FfiBackend;
use barretenberg_rs::generated_types::{CircuitInfoResponse, CircuitInput, ProofSystemSettings};
use outbe_zk::error::Error;

#[cfg(test)]
mod tests;

static HANDLER: OnceLock<BarretenbergHandler> = OnceLock::new();
// Native CRS initialization must not race before the handler's mutex exists.
static INITIALIZATION: Mutex<()> = Mutex::new(());

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
/// Call `init` during launch to complete blocking initialization before proof operations start.
/// Restart the process after a panic during a native operation.
/// Another reference to this interface does not create an independent engine or reset native state.
pub struct BarretenbergHandler {
    api: Mutex<BarretenbergApi<FfiBackend>>,
    crs: srs::Crs,
}

impl BarretenbergHandler {
    /// Initialize the handler and canonical CRS during launch, before proof operations start.
    pub fn init() -> Result<(), HandlerError> {
        catch_unwind(AssertUnwindSafe(|| {
            let _initialization = INITIALIZATION.lock().map_err(|_| HandlerError::Terminal)?;
            if HANDLER.get().is_none() {
                let handler = Self::new_instance()?;
                let _ = HANDLER.set(handler);
            }
            Ok(())
        }))
        .unwrap_or(Err(HandlerError::Initialization(CrsError::Panicked)))
    }

    /// Return the handler initialized during launch. Never initialize or load CRS data here.
    ///
    /// # Panics
    /// Panics if the application did not call `init` successfully before accessing the handler.
    pub fn global() -> &'static Self {
        HANDLER
            .get()
            .expect("BarretenbergHandler::init must complete during launch before global access")
    }

    fn new_instance() -> Result<Self, HandlerError> {
        let points = CANONICAL_SRS_POINTS;
        let backend = FfiBackend::new()
            .map_err(|error| command_error(BackendProofError::BbInit(error.to_string()).into()))?;
        let mut api = BarretenbergApi::new(backend);
        let (points_buf, num_points, g2_point) =
            load_srs(points).map_err(HandlerError::Initialization)?;
        api.srs_init_srs(&points_buf, num_points, &g2_point)
            .map_err(|e| HandlerError::Initialization(CrsError::SrsInit(e.to_string())))?;
        Ok(Self {
            api: Mutex::new(api),
            crs: srs::Crs::new(points),
        })
    }

    /// Check the fixed native CRS capacity. This method cannot increase the capacity.
    pub fn ensure_capacity(&self, points: u32) -> Result<(), HandlerError> {
        self.crs.ensure_capacity(points).map_err(command_error)
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
            self.crs
                .validate_circuit(api, &circuit.bytecode, &settings)?;
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
        raw: bool,
        settings: ProofSystemSettings,
    ) -> Result<bool, HandlerError> {
        execute(&self.api, |api| {
            let result = api
                .circuit_verify(&vk, public, proof, settings)
                .map_err(|error| {
                    if raw {
                        BackendProofError::BbRawVerify(error.to_string())
                    } else {
                        BackendProofError::BbVerify(error.to_string())
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
        operation(&mut state).map_err(command_error)
    }))
    .unwrap_or(Err(HandlerError::Terminal))
}

fn command_error(error: Error) -> HandlerError {
    match error {
        // Do not repeat the generic proof error's display prefix during conversion.
        Error::Proof(message) => HandlerError::Command(message),
        other => HandlerError::Command(other.to_string()),
    }
}
