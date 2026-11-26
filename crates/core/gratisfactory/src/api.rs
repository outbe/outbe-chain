//! Cross-module API for the Gratisfactory module.

use alloy_primitives::Address;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Re-exported so cross-module callers (e.g. `outbe_nodfactory`) can build the
/// caller's Gratis write authorization without depending on `outbe_gratis`.
pub use outbe_gratis::api::ModifyAuth;

#[cfg(any(test, feature = "test-utils"))]
pub fn mint(
    storage: StorageHandle<'_>,
    to: Address,
    amount: alloy_primitives::U256,
    auth: ModifyAuth,
) -> Result<()> {
    storage.with_checkpoint(|| crate::runtime::mint(storage.clone(), to, amount, auth))
}

/// Mint the exact encrypted NOD entitlement and acquire its Fidelity cohort.
pub fn mint_encrypted_nod(
    storage: StorageHandle<'_>,
    nod: &outbe_core_common::nod_encryption::EncryptedNodV1,
    auth: ModifyAuth,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let now = storage.now_secs()?;
        let section = outbe_fidelity::api::cohort_section(
            storage.clone(),
            nod.terms.owner,
            outbe_fidelity::api::FidelityCohortOp::In,
            now,
        )?;
        let outcome = outbe_gratis::api::mint_encrypted_nod(storage.clone(), nod, auth, section)?;
        outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), nod.terms.owner, &outcome)
    })
}

pub fn encrypted_balance(
    storage: StorageHandle<'_>,
    account: Address,
) -> Result<alloy_primitives::Bytes> {
    outbe_gratis::api::balance_ct(storage, account).map(Into::into)
}
