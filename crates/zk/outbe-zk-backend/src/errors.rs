//! Typed errors for the Barretenberg and ACVM proving backend.

use outbe_zk::error::Error;
use thiserror::Error;

/// Failures of the native handler for this process.
/// Restart the process after a terminal failure.
#[derive(Debug, Error)]
pub enum HandlerError {
    /// Native initialization failed. Launch must stop before proof processing starts.
    #[error("Barretenberg initialization failed: {0}")]
    Initialization(CrsError),
    /// A native operation panicked. Do not reuse the native state.
    #[error("Barretenberg handler failed; process restart required")]
    Terminal,
    /// A command failed. The handler can execute later operations.
    #[error("{0}")]
    Command(String),
}

impl From<HandlerError> for Error {
    fn from(error: HandlerError) -> Self {
        Self::Proof(error.to_string())
    }
}

/// Errors that CRS setup returns.
#[derive(Debug, Error)]
pub enum CrsError {
    /// Barretenberg SRS setup panicked across the FFI boundary.
    #[error("Barretenberg SRS initialization panicked")]
    Panicked,
    /// G1 SRS SHA-256 digest did not match the pinned digest for the point count.
    #[error(
        "SRS checksum mismatch at {num_points} points (tampered CRS?): got {got}, pinned {pinned}"
    )]
    SrsChecksumMismatch {
        /// Number of G1 points verified.
        num_points: u32,
        /// Actual hex-encoded SHA-256 digest.
        got: String,
        /// Pinned hex-encoded SHA-256 digest.
        pinned: String,
    },
    /// Failed to read local SRS file from disk.
    #[error("read SRS {}: {source}", path.display())]
    ReadSrs {
        /// Path to the SRS file.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Local cached SRS file has fewer bytes than required for the point count.
    #[error("cached SRS {} has {actual} bytes, need {need} for {num_points} points", path.display())]
    CachedSrsTooShort {
        /// Path to the cached SRS file.
        path: std::path::PathBuf,
        /// Actual byte length of the file.
        actual: usize,
        /// Required byte length.
        need: usize,
        /// Requested G1 point count.
        num_points: u32,
    },
    /// Failed to send SRS range download request.
    #[error("SRS download: {0}")]
    SrsDownload(String),
    /// Failed to read SRS range download response body.
    #[error("SRS download body: {0}")]
    SrsDownloadBody(String),
    /// Barretenberg global one-shot SRS was already initialized with fewer points.
    #[error("bb SRS already initialized to {initialized} points (one-shot) but this circuit needs {requested}; the handler uses the fixed canonical CRS capacity")]
    SrsAlreadyInitialized {
        /// Point count already initialized.
        initialized: u32,
        /// Point count requested by the current circuit.
        requested: u32,
    },
    /// Local SRS was unavailable and network SRS fallback is disabled at compile time.
    #[error("SRS not available at {} ({reason}); the network fallback is disabled (built without `with-network-srs`). Initialize the wallet with a bundled SRS file via set_srs_path() before proving.", path.display())]
    SrsUnavailableWithoutNetwork {
        /// Path checked for local SRS.
        path: std::path::PathBuf,
        /// Reason the local read failed.
        reason: String,
    },
    /// Barretenberg srs_init_srs FFI call failed.
    #[error("srs init: {0}")]
    SrsInit(String),
}

impl From<CrsError> for Error {
    fn from(error: CrsError) -> Self {
        Self::Proof(error.to_string())
    }
}

/// Errors produced by ACIR/ACVM witness solving and Barretenberg FFI proving/verification.
#[derive(Debug, Error)]
pub enum BackendProofError {
    /// Failed to base64-decode committed ACIR bytecode.
    #[error("acir bytecode base64: {0}")]
    AcirBytecodeBase64(#[source] base64::DecodeError),
    /// Failed to gunzip committed ACIR bytecode.
    #[error("acir decompress: {0}")]
    AcirDecompress(#[source] std::io::Error),
    /// Failed to deserialize ACIR program.
    #[error("acir deserialize: {0}")]
    AcirDeserialize(String),
    /// Deserialized ACIR program contains no circuit functions.
    #[error("acir program has no functions")]
    AcirProgramMissingFunctions,
    /// ACVM solver failed to solve the circuit witness.
    #[error("acvm solve: {0}")]
    AcvmSolve(String),
    /// Failed to serialize solved witness stack.
    #[error("witness serialize: {0}")]
    WitnessSerialize(String),
    /// Failed to gunzip serialized witness stack.
    #[error("witness decompress: {0}")]
    WitnessDecompress(#[source] std::io::Error),
    /// Failed to initialize the Barretenberg FFI backend.
    #[error("bb init: {0}")]
    BbInit(String),
    /// Barretenberg circuit_prove call failed.
    #[error("bb prove: {0}")]
    BbProve(String),
    /// Barretenberg circuit_verify call failed.
    #[error("bb verify: {0}")]
    BbVerify(String),
    /// Barretenberg raw circuit_verify call failed.
    #[error("bb raw verify: {0}")]
    BbRawVerify(String),
    /// Combined proof blob is shorter than the 4-byte public-input count header.
    #[error("combined proof shorter than 4-byte header")]
    CombinedProofHeaderTooShort,
    /// Combined proof blob is truncated inside the public-input section.
    #[error("combined proof truncated in public inputs")]
    CombinedProofTruncatedPublicInputs,
    /// Combined proof blob's proof section length is not a multiple of 32 bytes.
    #[error("combined proof: proof section not a multiple of 32 bytes")]
    CombinedProofMisalignedProofSection,
    /// Barretenberg circuit_stats call failed.
    #[error("bb circuit_stats: {0}")]
    BbCircuitStats(String),
}

impl From<BackendProofError> for Error {
    fn from(error: BackendProofError) -> Self {
        Self::Proof(error.to_string())
    }
}
