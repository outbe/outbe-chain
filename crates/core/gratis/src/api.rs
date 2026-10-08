//! Cross-module API for the confidential Gratis token.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use outbe_tee::protocol::{FidelityOpOutcome, FidelityOpSection, ModifyAuth};

use crate::runtime;
use crate::schema::Gratis;

// --- Reads ---

/// Encrypted balance blob for `account`. Decrypt it client-side with the view key.
pub fn balance_ct(storage: StorageHandle<'_>, account: Address) -> Result<Vec<u8>> {
    crate::state::account(&Gratis::new(storage), account).balance_ct()
}

/// Encrypted pledged-collateral blob for `account`. Decrypt it client-side with the view key.
pub fn pledged_ct(storage: StorageHandle<'_>, account: Address) -> Result<Vec<u8>> {
    crate::state::account(&Gratis::new(storage), account).pledged_ct()
}

/// The account's current modify-auth replay counter (the value the client's next
/// write authorization must bind).
pub fn op_nonce(storage: StorageHandle<'_>, account: Address) -> Result<u64> {
    crate::state::account(&Gratis::new(storage), account).op_nonce()
}

/// Public sum of every account's pledged balance.
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

/// Move an owner-authorized amount into the pledged balance, with a read-only
/// Fidelity eligibility probe.
pub fn pledge_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    runtime::pledge_with_fidelity(storage, caller, amount, auth, fidelity)
}

/// Return pledged collateral to `account`'s liquid balance. The caller authorizes it.
pub fn release_pledged(storage: &StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    runtime::release_pledged(storage, account, amount)
}

/// Burn `account`'s pledged collateral. The caller authorizes it.
pub fn burn_pledged(storage: &StorageHandle<'_>, account: Address, amount: U256) -> Result<()> {
    runtime::burn_pledged(storage, account, amount)
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
