//! Permissionless execution of governance-approved vault changes.

use alloy_primitives::U256;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{api::IVaultRouter, schema::VaultRouterContract, state};

pub(crate) fn apply_approved(storage: StorageHandle<'_>, proposal_id: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        let record = state::approved_vault_action(&storage, proposal_id)?;
        // Consume before any subcall. The checkpoint restores this on every failure.
        state::consume_approved_vault_action(&storage, &record)?;
        let now = u128::from(storage.block_number()?);
        let deadline = u128::from(record.approved_at)
            + u128::from(outbe_chain_constants::get_governance_voting_window_blocks());
        if now > deadline {
            return VaultRouterContract::new(storage.clone()).emit(
                IVaultRouter::ApprovedVaultActionExpired {
                    proposalId: proposal_id,
                    vault: record.vault,
                },
            );
        }
        match record.kind {
            state::ADD_VAULT => super::add_vault(storage.clone(), record.vault),
            state::REMOVE_VAULT => super::remove_vault(storage.clone(), record.vault),
            _ => Err(crate::errors::VaultRouterError::InvalidApprovedVaultAction.into()),
        }
    })
}
