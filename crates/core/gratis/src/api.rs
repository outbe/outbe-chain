//! Cross-module API for the confidential Gratis token.

use alloy_primitives::{Address, B256, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use outbe_tee::protocol::{FidelityOpOutcome, FidelityOpSection, ModifyAuth, PledgeTerms};

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

/// Pledge the gratis that covers `terms.stables_amount` from `caller` into a new
/// pending `PledgeLockTicket`, sealing `terms` alongside it. `amount_stables` is the
/// MAC-bound figure and must equal `terms.stables_amount`. Returns an owner-encrypted
/// pledge reply, which the owner opens to construct a fresh issuance credential.
pub fn pledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    terms: PledgeTerms,
    auth: ModifyAuth,
) -> Result<Vec<u8>> {
    runtime::pledge(storage, caller, amount_stables, terms, auth)
}

/// Pledge gratis AND carry a co-located fidelity **probe** in ONE round-trip.
/// Returns `(pledge_reply, fidelity_outcome)`; the outcome's `league` is the
/// caller's current league for the eligibility gate (nothing to persist - a
/// probe never mutates cohorts).
pub fn pledge_with_fidelity(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    terms: PledgeTerms,
    auth: ModifyAuth,
    fidelity: FidelityOpSection,
) -> Result<(Vec<u8>, FidelityOpOutcome)> {
    runtime::pledge_with_fidelity(storage, caller, amount_stables, terms, auth, fidelity)
}

/// Directly unpledge an unspent (pending) pledge (`pledge_note`) back to `caller`.
/// `amount_stables` is the stables figure the pledge was quoted for - the enclave
/// matches it against the ticket. Returns the gratis collateral credited back.
pub fn unpledge(
    storage: StorageHandle<'_>,
    caller: Address,
    amount_stables: U256,
    pledge_note: B256,
    auth: ModifyAuth,
) -> Result<U256> {
    runtime::unpledge(storage, caller, amount_stables, pledge_note, auth)
}

pub use outbe_tee::confidential::{CollateralAction, CollateralAuthorization};

pub fn consume_pledge(
    storage: StorageHandle<'_>,
    credis_id: U256,
    credential: Vec<u8>,
    smart_account: Address,
) -> Result<(PledgeTerms, B256)> {
    runtime::consume_pledge(storage, credis_id, credential, smart_account)
}
pub fn apply_collateral(
    storage: StorageHandle<'_>,
    authorization: CollateralAuthorization,
    fidelity_anchor: u64,
) -> Result<U256> {
    runtime::apply_collateral(storage, authorization, fidelity_anchor)
}
