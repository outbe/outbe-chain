//! Cross-module API for the confidential Gratis token.
//!
//! This is the surface other crates use to move Gratis. Owner-authorized writes
//! carry a [`ModifyAuth`] (the client's HMAC over the op + the account's current
//! `op_nonce`); the credis-driven entry points ([`pledge_to_bundle`],
//! [`unlock_to_eoa`]) are gated by the pledge-record state machine instead.
//! Reads return **ciphertext** — the caller decrypts with the account's view key
//! (see `outbe_tee_enclave::gratis::decrypt_balance`), never the host.
//!
//! Current production callers:
//! - `outbe_gratisfactory` — [`mine`]/[`burn`] acquisition & sale, [`pledge`]/
//!   [`unpledge`] the credis escrow.
//! - `outbe_nodfactory` — [`mine`] via the Nod acquisition path.
//! - `outbe_credisfactory` — [`pledge_to_bundle`] (requestCredis) and
//!   [`unlock_to_eoa`] (payAnadosis).

use alloy_primitives::{Address, B256, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use outbe_tee::protocol::ModifyAuth;

use crate::runtime;
use crate::schema::Gratis;

// --- Reads ---

/// Encrypted balance blob for `account`; decrypt client-side with the view key.
pub fn balance_ct(storage: StorageHandle<'_>, account: Address) -> Result<Vec<u8>> {
    Gratis::new(storage).balance_ct_of(account)
}

/// Encrypted pledged-ledger blob for `account`.
pub fn pledged_ct(storage: StorageHandle<'_>, account: Address) -> Result<Vec<u8>> {
    Gratis::new(storage).pledged_ct_of(account)
}

/// The account's current modify-auth replay counter (the value the client's next
/// write authorization must bind).
pub fn op_nonce(storage: StorageHandle<'_>, account: Address) -> Result<u64> {
    Gratis::new(storage).op_nonce_of(account)
}

/// Public total circulating supply (aggregate; per-account balances hidden).
pub fn total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    Gratis::new(storage).total_supply()
}

/// Public aggregate pledged into the credis escrow (per-account amounts hidden).
pub fn pledged_total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    Gratis::new(storage).pledged_total_supply()
}

// --- Owner-authorized mutations ---

/// Mint `amount` gratis to `caller`.
pub fn mine(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    runtime::mine(storage, caller, amount, auth)
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

/// Pledge `amount` gratis from `caller` over `installments` anadosis payments.
/// Returns the pledge handle to present at `requestCredis`.
pub fn pledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    installments: u32,
    auth: ModifyAuth,
) -> Result<B256> {
    runtime::pledge(storage, caller, amount, installments, auth)
}

/// Directly unpledge an unspent pledge (`pledge_handle`) back to `caller`.
pub fn unpledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount: U256,
    pledge_handle: B256,
    auth: ModifyAuth,
) -> Result<()> {
    runtime::unpledge(storage, caller, amount, pledge_handle, auth)
}

// --- Credis-driven ---

/// requestCredis: consume `pledge_handle` for `bundle` (authorized by
/// `spend_auth`) and return the pledged gratis amount.
pub fn pledge_to_bundle(
    storage: StorageHandle<'_>,
    pledge_handle: B256,
    bundle: Address,
    spend_auth: [u8; 32],
) -> Result<U256> {
    runtime::pledge_to_bundle(storage, pledge_handle, bundle, spend_auth)
}

/// payAnadosis: release one installment of `pledge_handle` back to `eoa`. Returns
/// the released amount.
pub fn unlock_to_eoa(
    storage: StorageHandle<'_>,
    eoa: Address,
    pledge_handle: B256,
) -> Result<U256> {
    runtime::unlock_to_eoa(storage, eoa, pledge_handle)
}
