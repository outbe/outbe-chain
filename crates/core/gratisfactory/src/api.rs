//! Cross-module API for the Gratisfactory module.

use alloy_primitives::Address;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

/// Re-exported so cross-module callers (e.g. `outbe_nodfactory`) can build the
/// caller's Gratis write authorization without depending on `outbe_gratis`.
pub use outbe_gratis::api::ModifyAuth;

pub use crate::runtime::{
    burn_from_credis, collateral_of, pledge_of, release_from_credis, send_to_credis,
};

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
    nod: &outbe_primitives::nod_encryption::EncryptedNodV1,
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

/// Validate the Gratis op-nonce and prepare the current Gratis balance ciphertext
/// and Fidelity `In` cohort section for a combined Promis->Gratis conversion.
pub fn prepare_mine_from_promis(
    storage: &StorageHandle<'_>,
    account: Address,
    gratis_op_nonce: u64,
) -> Result<(Vec<u8>, outbe_gratis::api::FidelityOpSection)> {
    outbe_gratis::api::check_op_nonce(storage, account, gratis_op_nonce)?;
    let now = storage.now_secs()?;
    let current_gratis_balance = outbe_gratis::api::balance_ct(storage.clone(), account)?;
    let section = outbe_fidelity::api::cohort_section(
        storage.clone(),
        account,
        outbe_fidelity::api::FidelityCohortOp::In,
        now,
    )?;
    Ok((current_gratis_balance, section))
}

/// Commit the Gratis balance, op-nonce, `GratisMinedFromPromis` event, and Fidelity
/// acquisition cohort from a combined Promis->Gratis enclave operation.
pub fn commit_mine_from_promis(
    storage: &StorageHandle<'_>,
    account: Address,
    gratis_op_nonce: u64,
    result: &outbe_gratis::api::MineGratisFromPromisResultV1,
) -> Result<()> {
    let outcome =
        outbe_gratis::api::apply_mined_from_promis(storage, account, gratis_op_nonce, result)?;
    outbe_fidelity::api::apply_fidelity_outcome(storage.clone(), account, &outcome)
}
