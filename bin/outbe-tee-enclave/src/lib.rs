//! `outbe-tee-enclave` - the TEE enclave core for the Tribute PoC.
//!
//! This crate holds the **secret-bearing** logic that runs only inside the
//! enclave: the offer-decryption primitive, the DKG -> tribute-offer-key
//! derivation chain, and the sealed-blob format. The host (`outbe-tee`) never
//! links the secret crypto. It only uses the neutral `outbe_tee::protocol`
//! message contract over a Noise-IK channel.
//!
//! SGX integration is real, not mocked: [`gramine`] talks to the actual
//! `/dev/attestation/*` surface. Under `gramine-sgx` the quote is a real DCAP
//! quote, measurements are parsed from it, and the sealing key comes from
//! `EGETKEY`. Under `gramine-direct` (no SGX hardware) there is no quote and no
//! `EGETKEY`, so the enclave reports `attestation_type=none` and runs in an
//! explicitly-unattested mode rather than fabricating attestation.
//!
//!   - [`crypto`] - ECDHE + HKDF + ChaCha20Poly1305 offer decrypt (byte-identical
//!     to the host's current scheme) and the tribute-offer-key derivation.
//!   - [`seal`]   - the `TSEAL` sealed-blob format. The sealing key is the real
//!     `EGETKEY` key under `gramine-sgx` (a `mock`-gated dev key only off-hardware).
//!   - [`gramine`] - the real `/dev/attestation/*` quote/seal/measurement surface.

pub mod dcap_verifier;
pub mod dkg;
pub mod errors;
pub mod finalized_admission;
pub mod gramine;
pub mod initialization;
pub mod keys;
#[cfg(feature = "local-e2e")]
pub mod local_e2e;
mod onboarding_upload;
pub mod run;
pub mod seal;
#[cfg(not(feature = "sgx-sealing-probe"))]
mod sgx_sealing;
#[cfg(feature = "sgx-sealing-probe")]
pub mod sgx_sealing;
pub mod telemetry;
#[cfg(any(test, feature = "mock"))]
pub mod test_utils;
pub mod transport;

pub use outbe_enclave::{
    compute, confidential, crypto, dev, encrypted_tribute_offer, fidelity, fidelity_cipher, gratis,
    gratis_cipher, nod_encryption, nod_mine, payload, process, promis, promis_cipher, tribute_day,
    tribute_encryption, zk_claim,
};
