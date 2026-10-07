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
    Gratis::new(storage).balance_ct_of(account)
}

/// Encrypted pledged-collateral blob for `account`. Decrypt it client-side with the view key.
pub fn pledged_ct(storage: StorageHandle<'_>, account: Address) -> Result<Vec<u8>> {
    Gratis::new(storage).pledged_ct_of(account)
}

/// The account's current modify-auth replay counter (the value the client's next
/// write authorization must bind).
pub fn op_nonce(storage: StorageHandle<'_>, account: Address) -> Result<u64> {
    Gratis::new(storage).op_nonce_of(account)
}

/// Public total circulating supply. The value is an aggregate. Per-account balances
/// stay hidden.
pub fn total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    Gratis::new(storage).total_supply()
}

/// Public sum of every account's pledged balance.
pub fn pledged_total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    Gratis::new(storage).pledged_total_supply()
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

/// Burn `amount` gratis from `caller`. Returns the remaining total supply.
pub fn burn(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
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
