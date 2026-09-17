//! Cross-module API for the confidential ledger. Owner reads use encrypted requests.
use crate::{runtime, schema::Gratis};
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};
pub use outbe_tee::pledgenote::{Outcome, Quote, Terms};
pub use outbe_tee::protocol::ModifyAuth;
use outbe_tee::{pledge_ledger, pledgenote::Command};
pub fn total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    Gratis::new(storage).total_supply()
}
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

/// Mint and record acquisition atomically in the same private journal entry.
pub fn mint_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    runtime::mint_with_fidelity(storage, caller, amount, auth)
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

/// Burn and record the sale atomically in the same private journal entry.
pub fn burn_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<U256> {
    runtime::burn_with_fidelity(storage, caller, amount, auth)
}

pub fn query(storage: &StorageHandle<'_>, envelope: Vec<u8>) -> Result<Vec<u8>> {
    Ok(pledge_ledger::execute(storage, Command::Query { envelope })?.encrypted_receipt)
}
pub fn create_note(
    storage: &StorageHandle<'_>,
    quote: Quote,
    terms: Terms,
    envelope: Vec<u8>,
) -> Result<Outcome> {
    pledge_ledger::execute(
        storage,
        Command::Create {
            quote,
            terms,
            envelope,
        },
    )
}
pub fn cancel_note(storage: &StorageHandle<'_>, envelope: Vec<u8>) -> Result<Outcome> {
    pledge_ledger::execute(storage, Command::Cancel { envelope })
}
pub fn use_note(
    storage: &StorageHandle<'_>,
    owner_sa: Address,
    envelope: Vec<u8>,
) -> Result<Outcome> {
    pledge_ledger::execute(storage, Command::Use { owner_sa, envelope })
}
pub fn release_collateral(
    storage: &StorageHandle<'_>,
    collateral_handle: B256,
    amount: U256,
) -> Result<()> {
    pledge_ledger::execute(
        storage,
        Command::Release {
            collateral_handle,
            amount,
        },
    )
    .map(|_| ())
}
pub fn forfeit_collateral(
    storage: &StorageHandle<'_>,
    collateral_handle: B256,
    amount: U256,
) -> Result<()> {
    pledge_ledger::execute(
        storage,
        Command::Forfeit {
            collateral_handle,
            amount,
        },
    )
    .map(|_| ())
}
