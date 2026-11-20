//! Domain-shared confidential ledger cryptography and witness envelopes.
//!
//! Domain crates depend on this module for shared ledger cryptography. Byte strings
//! for HKDF labels, offer salt, and witness response tags stay frozen.

mod confidential;
mod crypto;
#[cfg(any(test, feature = "dev-keys"))]
mod dev;
mod errors;
mod offer_encrypt;
mod response_auth;
mod v2_balance_decrypt;
mod witness_envelope;

pub use confidential::{
    derive_account_keys_message, domain_for, eip191_hash, event_amount_input_hash,
    AccountKeyDerivation, BalanceTransition, Domain, EnvelopeCipherSpec, Ledger,
    ModifyAuthorization, ModifyDomain, ModifyOperation, OperationTag, SlotCipher, SlotCipherDomain,
    FIDELITY, FIDELITY_COHORT_ENVELOPE, FIELD_BALANCE, GRATIS, GRATIS_BALANCE_ENVELOPE, NOD,
    PROMIS, PROMIS_BALANCE_ENVELOPE, TRIBUTE,
};
pub use crypto::{
    chacha20poly1305_decrypt, chacha20poly1305_encrypt, decrypt_tee_amount,
    ecdhe_tribute_offer_decrypt, hkdf_sha256,
};
#[cfg(feature = "dev-keys")]
pub use dev::encrypt_share;
#[cfg(any(test, feature = "dev-keys"))]
pub use dev::{
    decrypt_share, fidelity_chain, offer_key, offer_material, offer_public_key,
    CryptoResult as DevCryptoResult, EncryptedShare, FIDELITY_CHAIN_ID, FIDELITY_GROUP_SIG,
    GRATIS_CHAIN, GRATIS_GROUP_SIG, OFFER_SECRET_KEY,
};
pub use errors::{
    map_consensus_transport_error, parse_tribute_draft_id, BalanceClientError, ComputeError,
    DraftIdHexError, EnclaveClientError, LocalCryptoError, TeeError, TributeSourceError,
    FIDELITY_ATTESTATION, GRATIS_OP_ATTESTATION, NOD_MINT_REJECTED, PROMIS_OP_ATTESTATION,
    TRIBUTE_OFFER_ATTESTATION, TRIBUTE_SOURCE_REJECTED,
};
pub use offer_encrypt::{
    canary_offer_json, encrypt_tee_amount, encrypt_tee_amount_with, encrypt_tribute_offer,
    encrypt_tribute_offer_with, promis_mine_key_info, EncryptedOfferParts,
    ENCRYPTED_TEE_AMOUNT_LEN, OFFER_HKDF_INFO, OFFER_HKDF_SALT, PROMIS_MINE_HKDF_INFO,
};
pub use response_auth::{
    attestation, hash, sign_attestation_tag, verify_bound_witness_attestation,
    verify_response_attestation, verify_signed_domain, ResponseAuthSpec,
};
pub use v2_balance_decrypt::{
    decrypt_fidelity_cohorts, decrypt_gratis_balance, decrypt_nod_for_owner,
    decrypt_nod_quote_for_owner, decrypt_promis_balance, decrypt_tribute_for_owner,
};
pub use witness_envelope::{
    consensus_witness_call, decode_canonical_body, decode_tagged_payload,
    derive_resident_state_key, encode_canonical_body, encode_tagged_payload,
    seal_resident_account_keys, session_witness_call, try_derive_sealed_account_keys,
    CanonicalBodyDecoder, CanonicalBodyEncoder, DualWitnessAnswer, TaggedWitnessParts,
    TaggedWitnessRef, WitnessDomainWire, WitnessResponseTag, WitnessSchema, WitnessTagError,
    NON_RECORDABLE_V1_TAGS, V1_RESPONSE_TAG_MAX,
};
#[cfg(any(test, feature = "test-utils"))]
pub use witness_envelope::{pause_account_query_for_test, AccountQueryPauseGuard};
