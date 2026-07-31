//! `outbe-tee` — host-side TEE integration crate for the Tribute SGX enclave PoC.
//!
//! Architecture the DKG actor and all
//! gossip / ceremony bookkeeping live on the **node (host)**; the secret
//! material and key assembly live **inside the enclave**. This crate is the
//! host-side half: the neutral wire-protocol types, the framed-UDS + Noise-IK
//! codec, and the blocking client used from the precompile path.
//!
//! This crate MUST NOT contain secret-bearing cryptography — that lives only in
//! `bin/outbe-tee-enclave`. Here we keep the message contract and transport.

pub mod bootstrap;
pub mod client;
pub mod client_global;
pub mod codec;
pub mod dcap_protocol;
#[cfg(feature = "native-dcap")]
pub mod dcap_v1;
pub mod endorsement;
pub mod errors;
pub mod handoff;
#[cfg(feature = "native-dcap")]
pub mod native_qvl;
pub mod node_host;
pub mod protocol;
pub mod quote;
pub mod tee_dkg;

pub use bootstrap::{build_unsigned_bootstrap, BootstrapParams, EnclaveRegistration};
pub use client::{
    verify_gratis_op_attestation, verify_peer_quote, verify_promis_op_attestation,
    verify_tribute_offer_attestation, AttestedPeerKeys, AuthorizedEnclaveClient, EnclaveClient,
    EnclaveInitializationChallenge, GeneratedDcapQuoteV1, NodeHostNoiseKey, QuotePolicy,
};
pub use client_global::{
    install_authorized_enclave_client, install_enclave_client, is_enclave_configured,
    seal_offer_key_for_registry, try_with_enclave, verify_dcap_evidence_v1, RuntimeEnclaveClient,
};
pub use errors::TransportError;
pub use handoff::{
    answer_handoff_request, run_handoff_as_newcomer, HandoffEvent, HandoffGossip,
    HandoffWireMessage,
};
pub use node_host::{connect_or_initialize_validator_enclave, ValidatorNodeHostIdentityV1};
pub use tee_dkg::{CeremonyCoordinator, CeremonyOutcome, EnclaveChannel};

/// Noise pattern for the node <-> enclave channel: **IK** (the responder/enclave
/// static key is known to the initiator/host via the attested quote), with
/// X25519 + ChaChaPoly + SHA256.
pub const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_SHA256";

/// Fixed, **public** HKDF-SHA256 salt for the tribute offer encryption key.
///
/// An HKDF salt provides domain separation, not confidentiality — it is not a
/// secret. It is a single protocol constant (the same for every enclave and every
/// client), so the derived ChaCha20Poly1305 key is deterministic across all
/// validators. A client encrypts an offer with
/// `key = HKDF-SHA256(salt = OFFER_HKDF_SALT, ikm = ECDHE(ephemeral, tribute_offer_pub),
/// info = b"tribute-factory-encryption")` and ChaCha20Poly1305 over the JSON
/// payload — the only public input the client must know besides the on-chain
/// offer public key. Value: ASCII `"outbe/tribute/offer-salt/v1"`, zero-padded.
pub const OFFER_HKDF_SALT: [u8; 32] = *b"outbe/tribute/offer-salt/v1\0\0\0\0\0";
