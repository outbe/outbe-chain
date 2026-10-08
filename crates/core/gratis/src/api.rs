//! Cross-module API for the confidential Gratis token.

use alloy_primitives::{Address, B256, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use crate::context::unpledge_context;
pub use outbe_tee::protocol::{FidelityOpOutcome, FidelityOpSection, ModifyAuth};

use crate::runtime;
use crate::schema::Gratis;

// --- Reads ---

/// Encrypted balance blob for `account`. Decrypt it client-side with the view key.
pub fn balance_ct(storage: StorageHandle<'_>, account: Address) -> Result<Vec<u8>> {
    crate::state::account(&Gratis::new(storage), account).balance_ct()
}

/// The account's current modify-auth replay counter (the value the client's next
/// write authorization must bind).
pub fn op_nonce(storage: StorageHandle<'_>, account: Address) -> Result<u64> {
    crate::state::account(&Gratis::new(storage), account).op_nonce()
}

/// Public aggregate pledged into the credis escrow (per-account amounts hidden).
pub fn pledged_total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    crate::state::pledged_total_supply(&Gratis::new(storage))
}

// --- Owner-authorized mutations ---

/// Mint `amount` gratis to `caller`.
pub fn mint(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    runtime::mint(storage, caller, amount, auth)
}

/// Mint gratis AND apply a co-located fidelity cohort section in ONE enclave
/// round-trip. Returns the fidelity outcome for the caller to persist via
/// `outbe_fidelity::api::apply_fidelity_outcome`.
pub fn mint_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    runtime::mint_with_fidelity(storage, caller, amount, auth, fidelity)
}

/// Burn `amount` gratis from `caller`.
pub fn burn(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    runtime::burn(storage, caller, amount, auth)
}

/// Burn gratis AND apply a co-located fidelity cohort section in ONE enclave
/// round-trip. Returns the fidelity outcome for the caller to persist.
pub fn burn_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    runtime::burn_with_fidelity(storage, caller, amount, auth, fidelity)
}

/// Debit an owner-authorized amount with a read-only Fidelity eligibility probe.
pub fn pledge_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<(B256, FidelityOpOutcome)> {
    runtime::pledge_with_fidelity(storage, caller, amount, auth, fidelity)
}
pub fn unpledge(storage: StorageHandle<'_>, proof: &[u8]) -> Result<U256> {
    runtime::unpledge(storage, proof)
}
pub fn activate(storage: &StorageHandle<'_>, amount: U256) -> Result<()> {
    runtime::activate(storage, amount)
}
pub fn return_collateral(
    storage: &StorageHandle<'_>,
    position: U256,
    serial: B256,
    amount: U256,
    released_total: U256,
) -> Result<()> {
    runtime::return_collateral(storage, position, serial, amount, released_total)
}
pub fn forfeit(storage: &StorageHandle<'_>, amount: U256) -> Result<()> {
    runtime::forfeit(storage, amount)
}

/// Consume a canonical, settled encrypted NOD without exporting its amount.
pub fn mint_encrypted_nod(
    storage: StorageHandle<'_>,
    nod: &outbe_primitives::nod_encryption::EncryptedNodV2,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    runtime::mint_encrypted_nod(storage, nod, auth, fidelity)
}
