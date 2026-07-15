//! Cross-module API for NodFactory.
//!
//! Lysis calls [`issue_nod`] inside its lysis run; other modules can drive
//! mining via [`mine_gratis`] when they hold a `StorageHandle` and the
//! caller identity. The production ABI surface (only `mineGratis`) lives in
//! [`crate::precompile`].

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_nod::schema::NodIssueParams;

use crate::runtime;

/// Issue a new Nod. Validates parameters, deterministically derives the
/// nod id, writes entity state to Nod, and emits `NodIssued` at
/// `NOD_FACTORY_ADDRESS`. Returns the new nod id.
pub fn issue_nod(storage: &StorageHandle<'_>, params: &NodIssueParams) -> Result<U256> {
    runtime::issue_nod(storage, params)
}

/// Burn the caller-owned Nod after a PoW + qualification check,
/// pull `cost_amount_minor` of `asset` from the caller and deposit it into the
/// reserve vault provider (skipped when `cost_amount_minor == 0`), and mint the
/// matching gratis load to the caller. Returns the minted amount.
pub fn mine_gratis(
    storage: &StorageHandle<'_>,
    caller: Address,
    nod_id: U256,
    nonce: U256,
    asset: Address,
    auth: outbe_gratisfactory::api::ModifyAuth,
) -> Result<U256> {
    runtime::mine_gratis(storage, caller, nod_id, nonce, asset, auth)
}
