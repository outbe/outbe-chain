use alloy_primitives::Address;
use outbe_macros::contract;
use outbe_primitives::addresses::FIDELITY_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot, StorageBytes};

/// EVM storage layout for the Fidelity (RCFI) module.
///
/// The per-owner cohort ledger (the Gratis movement history) is **ciphertext at
/// rest**: the enclave is the only party that decrypts it. Cohorts are stored as
/// one AEAD blob per owner: `version(8, big-endian) || ciphertext`. This is the
/// same self-versioning shape Gratis uses for balances. The version feeds the
/// deterministic nonce, so an overwrite never reuses a `(key, nonce)` pair.
/// RCFI/league are never computed on-chain from this blob. The enclave produces
/// them (cohort ops, the per-WWD league snapshot, and signed queries).
///
/// The only plaintext scalar is `first_qualified_start`: the earliest
/// `qualified_start` across all accounts. It anchors the synthetic-max RCFI
/// ceiling for leagues (`maxFidelityIndexAt`). It is not attributable to any
/// account. League derivation needs it on-chain (and in the enclave, passed in),
/// so it stays in the clear. Timestamps are monotonic, so the first write is the
/// chain-wide minimum.
#[contract(addr = FIDELITY_ADDRESS)]
pub struct FidelityContract {
    // slot 0: encrypted per-owner cohort ledger blob (`version(8) || AEAD-ct`).
    // Empty when the owner has no cohort history.
    pub cohorts_ct: Mapping<Address, StorageBytes>,
    // slot 1: earliest qualified_start across all accounts. 0 = none qualified.
    pub first_qualified_start: Slot<u64>,
}
