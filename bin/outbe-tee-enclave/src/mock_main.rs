//! `outbe-tee-enclave-mock` - dev/CI mock enclave binary.
//!
//! Built ONLY with `--features mock` (enforced by `required-features` in
//! `Cargo.toml`), so the production `outbe-tee-enclave` binary links none of the
//! mock key material. It runs the same `run` entrypoint as production, and the
//! node talks to it over the same Noise-IK channel. It differs from production in:
//!   - a stable EGETKEY-equivalent sealing key (the `mock` feature). This key lets
//!     tests exercise the sealed restart fast-path under gramine-direct, where
//!     real `EGETKEY` is unavailable.
//!   - development initialization mode. It authorizes every command, so it
//!     bypasses the production command-authorization matrix.
//!   - flags. It requires `--dev-network-binding` and does not require `--tee-dir`.
//!   - the identity seed. It comes from `--dkg-seed`, `TEE_DEV_DKG_SEED` or a fixed
//!     test seed. `TEE_DEV_OFFER_SECRET` can also set the offer secret.
//!   - a loud `MOCK ENCLAVE` startup banner ([`RunOpts::mock`]).
//!
//! There is no fabricated SGX quote. It runs unattested (empty quote), and the
//! host's development transport accepts that. Use for localnet/CI
//! without SGX hardware. Never use in production.

use outbe_tee_enclave::run::{run, RunOpts};

/// Same heap accounting as the production binary so mock-lane e2e observes the
/// identical `Health` surface.
#[global_allocator]
static ALLOCATOR: outbe_tee_enclave::telemetry::CountingAllocator =
    outbe_tee_enclave::telemetry::CountingAllocator;

fn main() {
    std::process::exit(run(RunOpts::mock()));
}
