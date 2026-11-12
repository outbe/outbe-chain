/// Fixed dev identity for the in-process test-enclave stand-ins (NOT production).
///
/// In the real enclave one resident group signature yields every ledger's state
/// key (the transport derives the gratis, fidelity, ... keys from the same
/// `group_sig`). The per-crate in-process test enclaves must model that. The
/// *gratis* stand-in derives a fidelity key when it applies a folded cohort
/// section. That key MUST equal the key the *fidelity* stand-in uses for
/// snapshots/queries. Otherwise a folded mint would write a blob the snapshot
/// cannot read. Both derive the fidelity key from this single `(group_sig, chain)`
/// pair.
use alloy_primitives::{B256, U256};
use x25519_dalek::{PublicKey, StaticSecret};

/// Shared dev group signature for the fidelity test-enclave key.
pub const FIDELITY_GROUP_SIG: &[u8] = b"outbe-dev-fidelity-group-signature-fixed-seed";
/// Shared dev chain id for the fidelity test-enclave key (also the resident
/// chain the fidelity query auth is scoped to in tests).
pub const FIDELITY_CHAIN_ID: u64 = outbe_primitives::chain::DEVNET_CHAIN_ID;
/// Shared dev group signature for the gratis test-enclave key.
pub const GRATIS_GROUP_SIG: &[u8] = b"outbe-dev-gratis-group-signature-fixed-seed!!";
/// Shared dev chain id for the gratis test-enclave key.
pub const GRATIS_CHAIN: B256 = B256::repeat_byte(0xC1);
/// Shared dev X25519 secret key for TEE-encrypted amount envelopes in test-enclave mode.
pub const OFFER_SECRET_KEY: [u8; 32] = [0x42; 32];

/// The fidelity dev chain id as a `B256` (matching the runtime's
/// `B256::from(U256::from(storage.chain_id()))` encoding).
#[must_use]
pub fn fidelity_chain() -> B256 {
    B256::from(U256::from(FIDELITY_CHAIN_ID))
}

/// Public key corresponding to [`OFFER_SECRET_KEY`].
#[must_use]
pub fn offer_public_key() -> [u8; 32] {
    let secret = StaticSecret::from(OFFER_SECRET_KEY);
    PublicKey::from(&secret).to_bytes()
}
