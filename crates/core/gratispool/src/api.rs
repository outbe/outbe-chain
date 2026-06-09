//! Cross-module API for the shielded gratis pool.
//!
//! All four entry points are **purely cryptographic** — they do not move
//! Gratis balances or touch the per-account pledged ledger. Callers
//! orchestrate the Gratis-side bookkeeping:
//!
//! - `outbe_gratisfactory::pledge_gratis` calls [`add_commitment`] +
//!   `Gratis::pledge_to_pool`.
//! - `outbe_gratisfactory::unpledge_gratis` calls
//!   [`verify_and_spend_for_unpledge`] + `Gratis::unpledge_from_pool`.
//! - `outbe_credisfactory::request_credis` calls
//!   [`verify_and_spend_for_credis`] + `Gratis::bind_pool_to_credis`.
//! - `outbe_credisfactory::pay_anadosis` (final installment) calls
//!   `Gratis::unbind_pool_from_credis` + [`insert_reclaim`].

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::runtime::SpendArgs;

/// User-pledge entrypoint. See [`runtime::add_commitment`].
pub fn add_commitment(
    storage: StorageHandle<'_>,
    denom_id: u8,
    commitment: U256,
) -> Result<(U256, u32, U256)> {
    runtime::add_commitment(storage, denom_id, commitment)
}

/// `requestCredis` spend path. See [`runtime::verify_and_spend_for_credis`].
///
/// `nonce` is the application-derived context-binding payload folded into
/// `receiver_binding` (for `credisfactory::requestCredis` that is
/// `args.reclaim_commitment`).
pub fn verify_and_spend_for_credis(
    storage: StorageHandle<'_>,
    caller: Address,
    nonce: U256,
    args: &SpendArgs,
) -> Result<U256> {
    runtime::verify_and_spend_for_credis(storage, caller, nonce, args)
}

/// `unpledgeGratis` spend path. See [`runtime::verify_and_spend_for_unpledge`].
pub fn verify_and_spend_for_unpledge(
    storage: StorageHandle<'_>,
    destination: Address,
    args: &SpendArgs,
) -> Result<U256> {
    runtime::verify_and_spend_for_unpledge(storage, destination, args)
}
