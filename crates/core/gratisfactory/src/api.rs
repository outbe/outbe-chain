//! Cross-module API for the Gratisfactory module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Re-exported so cross-module callers (e.g. `outbe_nodfactory`) can build the
/// caller's Gratis write authorization without depending on `outbe_gratis`.
pub use outbe_gratis::api::ModifyAuth;

/// Mint `amount` gratis to `account` (authorized by the account owner's modify
/// key) and record the Fidelity acquisition cohort.
/// See [`crate::runtime::mint`].
pub fn mint(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    auth: ModifyAuth,
) -> Result<()> {
    crate::runtime::mint(storage, account, amount, auth)
}

/// Mint the exact encrypted NOD entitlement and acquire its Fidelity cohort.
pub fn mint_encrypted_nod(
    storage: StorageHandle<'_>,
    nod: &outbe_primitives::nod_encryption::EncryptedNodV2,
    auth: ModifyAuth,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let now = storage.timestamp()?.to::<u64>();
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
