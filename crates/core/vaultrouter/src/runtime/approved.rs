//! Permissionless execution of governance-approved vault changes.

use alloy_primitives::U256;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{api::IVaultRouter, schema::VaultRouterContract, state};

pub(crate) fn approved_vault_action(
    storage: &StorageHandle<'_>,
    proposal_id: U256,
) -> Result<IVaultRouter::approvedVaultActionReturn> {
    if !VaultRouterContract::new(storage.clone())
        .approved_vault_actions
        .exists(proposal_id)?
    {
        return Ok(IVaultRouter::approvedVaultActionReturn {
            action: IVaultRouter::VaultAction::Unknown,
            vault: alloy_primitives::Address::ZERO,
            unlockAt: 0,
        });
    }
    let record = state::approved_vault_action(storage, proposal_id)?;
    Ok(IVaultRouter::approvedVaultActionReturn {
        action: IVaultRouter::VaultAction::try_from(record.kind)
            .map_err(|_| crate::errors::VaultRouterError::InvalidApprovedVaultAction)?,
        vault: record.vault,
        unlockAt: record.unlock_at,
    })
}

pub(crate) fn apply_approved(storage: StorageHandle<'_>, proposal_id: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        let record = state::approved_vault_action(&storage, proposal_id)?;
        let unlock_height = record.unlock_at;
        let expiry = unlock_height
            .saturating_add(outbe_chain_constants::get_governance_voting_window_blocks());
        let now = storage.block_number()?;
        if now < unlock_height {
            return Err(crate::errors::VaultRouterError::ApprovedVaultActionLocked {
                proposal_id,
                unlock_height,
            }
            .into());
        }
        // Consume before any subcall. The checkpoint restores this on every failure.
        state::consume_approved_vault_action(&storage, &record)?;
        if now > expiry {
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
