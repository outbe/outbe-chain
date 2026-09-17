//! `outbe-tee-enclave` - the TEE enclave core for the Tribute PoC.
//!
//! This crate holds the **secret-bearing** logic that runs only inside the
//! enclave: the offer-decryption primitive, the DKG -> tribute-offer-key
//! derivation chain, and the sealed-blob format. The host (`outbe-tee`) never
//! links the secret crypto - it only speaks the neutral `outbe_tee::protocol`
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
//!   - [`seal`]   - the `TSEAL` sealed-blob format; the sealing key is the real
//!     `EGETKEY` key under `gramine-sgx` (a `mock`-gated dev key only off-hardware).
//!   - [`gramine`] - the real `/dev/attestation/*` quote/seal/measurement surface.

pub mod compute;
pub mod confidential;
pub mod crypto;
pub mod dcap_verifier;
pub mod dkg;
pub mod errors;
pub mod fidelity;
pub mod finalized_admission;
pub mod gramine;
pub mod gratis;
pub mod initialization;
pub mod keys;
mod onboarding_upload;
pub mod payload;
pub mod pledgenote;
pub mod process;
pub mod promis;
pub mod run;
pub mod seal;
pub mod telemetry;
pub mod transport;
pub mod zk_claim;

/// Shared identity and in-process transport for private-ledger tests.
/// Gratis and Fidelity adapters use the same state key and ledger implementation.
pub mod dev {
    use alloy_primitives::{B256, U256};

    pub const PLEDGE_CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;

    /// Matches the runtime's U256 chain-id encoding.
    #[must_use]
    pub fn pledge_chain() -> B256 {
        B256::from(U256::from(PLEDGE_CHAIN_ID))
    }

    pub const PLEDGE_OFFER_SECRET: [u8; 32] = [9; 32];

    pub fn pledge_state_key() -> crate::errors::Result<[u8; 32]> {
        crate::gratis::derive_gratis_state_key(
            b"outbe-dev-gratis-group-signature-fixed-seed!!",
            B256::repeat_byte(0xC1),
            0,
        )
    }

    /// In-process transport adapter used only by host crates' test backends.
    /// It operates on a caller-owned ledger and never the resident production cache.
    pub fn pledge_request(
        ledger: &mut crate::pledgenote::Ledger,
        request: &outbe_tee::protocol::EnclaveRequest,
    ) -> Result<outbe_tee::pledgenote::Response, String> {
        use outbe_tee::{pledgenote::*, protocol::EnclaveRequest};
        let key = pledge_state_key().map_err(|e| e.to_string())?;
        let (inputs_hash, reply) = match request {
            EnclaveRequest::ApplyPledgeLedger { request } => (
                request_hash(request)?,
                ledger.apply(&key, &PLEDGE_OFFER_SECRET, request)?,
            ),
            EnclaveRequest::ReplayPledgeLedger { request } => {
                if request.reset {
                    *ledger = Default::default();
                }
                let reply = if ledger.head() != request.parent {
                    Reply::NeedsReplay
                } else {
                    ledger.replay(&key, &PLEDGE_OFFER_SECRET, &request.entries)?;
                    Reply::Applied(Box::new(Outcome {
                        head: ledger.head(),
                        ..Default::default()
                    }))
                };
                (replay_hash(request)?, reply)
            }
            _ => return Err("unexpected test ledger request".into()),
        };
        Ok(Response {
            inputs_hash,
            reply,
            attestation: Vec::new(),
        })
    }
}
