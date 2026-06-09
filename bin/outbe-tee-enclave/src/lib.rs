//! `outbe-tee-enclave` — the TEE enclave core for the Tribute PoC.
//!
//! This crate holds the **secret-bearing** logic that runs only inside the
//! enclave: the offer-decryption primitive, the DKG -> tribute-offer-key
//! derivation chain, and the sealed-blob format. The host (`outbe-tee`) never
//! links the secret crypto — it only speaks the neutral `outbe_tee::protocol`
//! message contract over a Noise-IK channel.
//!
//! SGX integration is real, not mocked: [`gramine`] talks to the actual
//! `/dev/attestation/*` surface. Under `gramine-sgx` the quote is a real DCAP
//! quote, measurements are parsed from it, and the sealing key comes from
//! `EGETKEY`. Under `gramine-direct` (no SGX hardware) there is no quote and no
//! `EGETKEY`, so the enclave reports `attestation_type=none` and runs in an
//! explicitly-unattested mode rather than fabricating attestation.
//!
//!   - [`crypto`] — ECDHE + HKDF + ChaCha20Poly1305 offer decrypt (byte-identical
//!     to the host's current scheme) and the tribute-offer-key derivation.
//!   - [`seal`]   — the `TSEAL` sealed-blob format; the sealing key is the real
//!     `EGETKEY` key under `gramine-sgx` (a `mock`-gated dev key only off-hardware).
//!   - [`gramine`] — the real `/dev/attestation/*` quote/seal/measurement surface.

pub mod compute;
pub mod crypto;
pub mod dkg;
pub mod errors;
pub mod gramine;
pub mod keys;
pub mod payload;
pub mod process;
pub mod run;
pub mod seal;
pub mod transport;
