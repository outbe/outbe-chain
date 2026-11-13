//! Domain enclave protocol, host clients, owner-local cryptography, and in-enclave engines.
//!
//! - Default `host` feature: lightweight domain wire protocol (`protocol`), host
//!   client wrappers, client-side offer encryption (`offer_encrypt`), and
//!   owner-local decryption (`*_decrypt`) over `outbe-tee`'s domain-neutral
//!   `Payload` transport and witness tape.
//! - `engine` feature: secret-bearing in-enclave business logic (`gratis`,
//!   `promis`, `fidelity`, `tribute_day`, `nod_encryption`, `nod_mine`,
//!   `process`, `encrypted_tribute_offer`, `zk_claim`, `seal`, `crypto`).
//! - `dev-keys` feature: fixed test/dev group signatures and chain constants (`dev`).

#[cfg(any(test, feature = "engine"))]
pub mod errors;
#[cfg(feature = "host")]
pub mod host;

#[cfg(any(test, feature = "engine"))]
pub mod compute;
#[cfg(any(test, feature = "engine"))]
pub mod confidential;
#[cfg(any(test, feature = "engine"))]
pub mod crypto;
#[cfg(any(test, feature = "dev-keys"))]
pub mod dev;
#[cfg(any(test, feature = "engine"))]
pub mod encrypted_tribute_offer;
#[cfg(any(test, feature = "engine"))]
pub mod fidelity;
#[cfg(any(test, feature = "engine"))]
pub mod fidelity_cipher;
#[cfg(any(test, feature = "engine"))]
pub mod gratis;
#[cfg(any(test, feature = "engine"))]
pub mod gratis_cipher;
#[cfg(any(test, feature = "engine"))]
pub mod nod_encryption;
#[cfg(any(test, feature = "engine"))]
#[path = "nod_mine.rs"]
mod nod_mine_engine;
#[cfg(any(test, feature = "engine"))]
pub mod payload;
#[cfg(any(test, feature = "engine"))]
pub mod process;
#[cfg(any(test, feature = "engine"))]
pub mod promis;
#[cfg(any(test, feature = "engine"))]
pub mod promis_cipher;
#[cfg(any(test, feature = "engine"))]
pub mod seal;
#[cfg(any(test, feature = "engine"))]
#[path = "tribute_day.rs"]
mod tribute_day_engine;
#[cfg(any(test, feature = "engine"))]
pub mod tribute_encryption;
#[cfg(any(test, feature = "engine"))]
pub mod zk_claim;

#[cfg(feature = "host")]
pub use host::{
    balance_client, canary_domain_request, consensus_domain_call, decrypt_promis_balance,
    domain_session_call, domain_session_call_on, fidelity_decrypt, gratis_decrypt, nod_decrypt,
    offer_encrypt, promis_decrypt, protocol, request_authorized_enclave_client,
    request_authorized_enclave_client_on_lane, request_enclave_client,
    request_enclave_client_on_lane, request_enclave_session, request_enclave_session_on_lane,
    request_runtime_enclave_client, response_auth, tribute_client, tribute_day_client,
    tribute_decrypt, tribute_source, tribute_v2, try_domain_account_query, try_with_domain_enclave,
    verification, verify_fidelity_cohort_attestation, verify_fidelity_query_attestation,
    verify_fidelity_snapshot_attestation, verify_gratis_op_attestation,
    verify_mine_gratis_from_promis_attestation, verify_promis_op_attestation,
    verify_tribute_offer_attestation, BalanceClientError, EnclaveClientError, LocalCryptoError,
    TributeSourceError, OFFER_HKDF_SALT,
};

/// Confidential NOD mining protocol types, host client, and (with `engine`) execution.
#[cfg(any(test, feature = "host", feature = "engine"))]
pub mod nod_mine {
    #[cfg(feature = "host")]
    pub use crate::host::nod_mine::*;
    #[cfg(any(test, feature = "engine"))]
    pub use crate::nod_mine_engine::*;
}

/// Encrypted Tribute day arithmetic protocol types and (with `engine`) execution.
#[cfg(any(test, feature = "host", feature = "engine"))]
pub mod tribute_day {
    #[cfg(feature = "host")]
    pub use crate::host::tribute_day::*;
    #[cfg(any(test, feature = "engine"))]
    pub use crate::tribute_day_engine::*;
}
