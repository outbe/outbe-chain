//! Single native owner: the dispatcher and CRS are process-global, not reentrant.
//! Never bypass this module with another FFI backend or a second native worker.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};

use barretenberg_rs::api::BarretenbergApi;
use barretenberg_rs::backends::FfiBackend;
use barretenberg_rs::generated_types::{CircuitInfoResponse, CircuitInput, ProofSystemSettings};
use outbe_zk::error::Error;

use super::{configure_memory, srs, Barretenberg, CANONICAL_SRS_POINTS};
use crate::errors::{BackendProofError, CrsInitError, HandlerError};

#[cfg(test)]
mod tests;

const QUEUE_CAPACITY: usize = 32;
static HANDLER: OnceLock<Result<BarretenbergHandler, HandlerError>> = OnceLock::new();

struct Envelope<Q, R> {
    request: Q,
    reply: mpsc::SyncSender<Result<R, HandlerError>>,
}

// Shared by production and fake-executor tests; the executor itself never leaves
// its OS thread, so neither it nor the native API needs to implement Send.
struct Worker<Q, R> {
    sender: mpsc::SyncSender<Envelope<Q, R>>,
    failed: Arc<AtomicBool>,
}

impl<Q: Send + 'static, R: Send + 'static> Worker<Q, R> {
    fn start<F, E>(capacity: usize, initialize: F) -> Result<Self, HandlerError>
    where
        F: FnOnce() -> Result<E, HandlerError> + Send + 'static,
        E: FnMut(Q) -> Result<R, Error> + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel::<Envelope<Q, R>>(capacity);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = Arc::clone(&failed);
        std::thread::Builder::new()
            .name("barretenberg-owner".into())
            .spawn(move || {
                let mut execute = match catch_unwind(AssertUnwindSafe(initialize)) {
                    Ok(Ok(execute)) => execute,
                    Ok(Err(error)) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                    Err(_) => {
                        let _ = ready_tx
                            .send(Err(HandlerError::Initialization(CrsInitError::Panicked)));
                        return;
                    }
                };
                if ready_tx.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(envelope) = receiver.recv() {
                    match catch_unwind(AssertUnwindSafe(|| execute(envelope.request))) {
                        Ok(result) => {
                            let _ = envelope.reply.send(result.map_err(|error| match error {
                                // Preserve the existing generic proof-error boundary without
                                // adding its display prefix again when the facade converts back.
                                Error::Proof(message) => HandlerError::Command(message),
                                other => HandlerError::Command(other.to_string()),
                            }));
                        }
                        Err(_) => {
                            worker_failed.store(true, Ordering::Release);
                            let _ = envelope.reply.send(Err(HandlerError::Terminal));
                            // Dropping the receiver releases queued and blocked callers.
                            return;
                        }
                    }
                }
            })
            .map_err(|error| HandlerError::Spawn(error.to_string()))?;
        ready_rx
            .recv()
            .map_err(|_| HandlerError::ResponseDisconnected)??;
        Ok(Self { sender, failed })
    }

    fn call(&self, request: Q) -> Result<R, HandlerError> {
        let disconnected = |fallback| {
            if self.failed.load(Ordering::Acquire) {
                HandlerError::Terminal
            } else {
                fallback
            }
        };
        if self.failed.load(Ordering::Acquire) {
            return Err(HandlerError::Terminal);
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.sender
            .send(Envelope { request, reply })
            .map_err(|_| disconnected(HandlerError::RequestDisconnected))?;
        response
            .recv()
            .map_err(|_| disconnected(HandlerError::ResponseDisconnected))?
    }
}

enum Request {
    Prove {
        circuit: CircuitInput,
        witness: Vec<u8>,
        options: Barretenberg,
    },
    Verify {
        vk: Vec<u8>,
        public: Vec<Vec<u8>>,
        proof: Vec<Vec<u8>>,
        acir: Option<Vec<u8>>,
        settings: ProofSystemSettings,
    },
    Stats {
        circuit: CircuitInput,
        include_gates: bool,
        settings: ProofSystemSettings,
    },
    Capacity(u32),
}

enum Response {
    Proof(Vec<Vec<u8>>),
    Verified(bool),
    Stats(CircuitInfoResponse),
    Capacity,
}

/// Process-wide facade for one retained native API on a dedicated OS thread.
///
/// All methods block, including queue backpressure and readiness. Async callers
/// should use their runtime's blocking-task facility. The 32-entry queue does
/// not bound memory held by blocked producers. Concurrent submissions serialize
/// complete operations; they do not run native verification concurrently.
/// Mutable API borrowing protects one object; the singleton prevents independent
/// objects from racing the native dispatcher's global state.
///
/// Configure CRS path/capacity and `HARDWARE_CONCURRENCY` before initialization.
/// For example, launch with `HARDWARE_CONCURRENCY=8` (a positive integer within
/// native bounds). The pinned native implementation caches concurrency per
/// thread, defaults to `min(32, detected concurrency)`, and uses one CPU in
/// `NO_MULTITHREADING` builds. More threads may help proving kernels or hurt
/// performance; verification throughput requires measurement. This does not
/// make the dispatcher reentrant or parallelize Rust Poseidon2. Never change
/// this environment variable while requests are running.
///
/// Initialization failure or worker panic requires process restart. Obtaining
/// another facade never constructs an independent engine or resets native state.
pub struct BarretenbergHandler {
    worker: Worker<Request, Response>,
}

impl BarretenbergHandler {
    /// Initialize the canonical capacity if needed and wait for readiness.
    pub fn global() -> Result<&'static Self, HandlerError> {
        Self::initialize(CANONICAL_SRS_POINTS)
    }

    /// Obtain an initialized handler without loading or downloading CRS.
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
                let worker = Worker::start(QUEUE_CAPACITY, move || {
                    if let Ok(path) = std::env::var("OUTBE_BB_SRS_PATH") {
                        srs::set_srs_path(path.into());
                    }
                    let backend = FfiBackend::new().map_err(|error| {
                        HandlerError::Initialization(CrsInitError::Failed(error.to_string()))
                    })?;
                    let mut api = BarretenbergApi::new(backend);
                    srs::ensure_srs(&mut api, points)
                        .map_err(|error| HandlerError::Initialization(error.into()))?;
                    Ok(move |request| execute(&mut api, request))
                })?;
                Ok(Self { worker })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Validate the fixed native CRS capacity. This cannot grow it.
    pub fn ensure_capacity(&self, points: u32) -> Result<(), HandlerError> {
        match self.worker.call(Request::Capacity(points))? {
            Response::Capacity => Ok(()),
            _ => Err(HandlerError::ResponseDisconnected),
        }
    }

    /// Run native circuit statistics on the same owner as proving/verification.
    /// This blocks and requires successful CRS initialization, even for diagnostics.
    pub fn circuit_stats(
        &self,
        circuit: CircuitInput,
        include_gates: bool,
        settings: ProofSystemSettings,
    ) -> Result<CircuitInfoResponse, HandlerError> {
        match self.worker.call(Request::Stats {
            circuit,
            include_gates,
            settings,
        })? {
            Response::Stats(info) => Ok(info),
            _ => Err(HandlerError::ResponseDisconnected),
        }
    }

    pub(super) fn prove(
        &self,
        circuit: CircuitInput,
        witness: Vec<u8>,
        options: Barretenberg,
    ) -> Result<Vec<Vec<u8>>, HandlerError> {
        match self.worker.call(Request::Prove {
            circuit,
            witness,
            options,
        })? {
            Response::Proof(proof) => Ok(proof),
            _ => Err(HandlerError::ResponseDisconnected),
        }
    }

    pub(super) fn verify(
        &self,
        vk: Vec<u8>,
        public: Vec<Vec<u8>>,
        proof: Vec<Vec<u8>>,
        acir: Option<Vec<u8>>,
        settings: ProofSystemSettings,
    ) -> Result<bool, HandlerError> {
        match self.worker.call(Request::Verify {
            vk,
            public,
            proof,
            acir,
            settings,
        })? {
            Response::Verified(verified) => Ok(verified),
            _ => Err(HandlerError::ResponseDisconnected),
        }
    }
}

fn execute(api: &mut BarretenbergApi<FfiBackend>, request: Request) -> Result<Response, Error> {
    match request {
        Request::Prove {
            circuit,
            witness,
            options,
        } => {
            configure_memory(options.low_memory, options.max_storage_usage);
            let settings = super::settings_ultra_honk_keccak(options.disable_zk);
            srs::ensure_srs_for(api, &circuit.bytecode, &settings)?;
            let result = api
                .circuit_prove(circuit, &witness, settings)
                .map_err(|error| BackendProofError::BbProve(error.to_string()))?;
            Ok(Response::Proof(result.proof))
        }
        Request::Verify {
            vk,
            public,
            proof,
            acir,
            settings,
        } => {
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
            Ok(Response::Verified(result.verified))
        }
        Request::Stats {
            circuit,
            include_gates,
            settings,
        } => Ok(Response::Stats(
            api.circuit_stats(circuit, include_gates, settings)
                .map_err(|error| BackendProofError::BbCircuitStats(error.to_string()))?,
        )),
        Request::Capacity(points) => {
            srs::ensure_srs(api, points)?;
            Ok(Response::Capacity)
        }
    }
}
