//! Cross-module API for the confidential Gratis token.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use crate::protocol::ModifyAuth;
pub use outbe_fidelity::{FidelityOpOutcome, FidelityOpSection};

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
#[cfg(any(test, feature = "test-utils"))]
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

/// Open the sealed pledge of `credis_reservation_id` and pledge `amount` from the main account
/// it names. Returns that account.
pub fn pledge_sealed(
    storage: &StorageHandle<'_>,
    sealed: &[u8],
    credis_reservation_id: U256,
    amount: U256,
) -> Result<Address> {
    runtime::pledge_sealed(storage, sealed, credis_reservation_id, amount)
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
    nod: &outbe_core_common::nod_encryption::EncryptedNodV1,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<FidelityOpOutcome> {
    runtime::mint_encrypted_nod(storage, nod, auth, fidelity)
}
