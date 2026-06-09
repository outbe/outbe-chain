//! `outbe-tee-enclave` binary: framed-UDS + Noise-IK server for the Tribute PoC.
//!
//! Usage: `outbe-tee-enclave --socket <path|host:port> [--dkg-seed <hex32>]`
//!        `[--tee-dir <dir>] [--chain-id <hex32>]`
//!
//! `--tee-dir` enables sealed-root persistence (`<tee-dir>/sealed_root.bin`);
//! `--chain-id` binds the seal AAD. Both are optional;
//! absent → sealing disabled and the offer key is re-derived from the DKG.
//!
//! The server binds a Unix domain socket (mode 0600), advertises its attested
//! keys via `GetQuote`, runs the Noise-IK handshake, and serves encrypted
//! requests (`GetPublicKeys`, `ProcessTributeOfferBatch`, ...).
//!
//! This is the production entrypoint — a thin shim over [`outbe_tee_enclave::run`]
//! with [`RunOpts::prod`] (no mock code). The dev mock binary
//! (`outbe-tee-enclave-mock`, `--features mock`) is the sibling shim.

use outbe_tee_enclave::run::{run, RunOpts};

fn main() {
    std::process::exit(run(RunOpts::prod()));
}
