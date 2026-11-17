//! Permissionless application of vault actions. A vault action is the record of an
//! approved proposal to add or remove a vault.

use alloy_primitives::U256;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{precompile::IVaultRouter, schema::VaultRouterContract, state};

pub(crate) fn get_vault_action(
    storage: &StorageHandle<'_>,
    proposal_id: U256,
) -> Result<IVaultRouter::getVaultActionReturn> {
    if !VaultRouterContract::new(storage.clone())
        .vault_actions
        .exists(proposal_id)?
    {
        return Ok(IVaultRouter::getVaultActionReturn {
            action: IVaultRouter::VaultAction::Unknown,
            vault: alloy_primitives::Address::ZERO,
            createdAt: 0,
        });
    }
    let record = state::vault_action(storage, proposal_id)?;
    Ok(IVaultRouter::getVaultActionReturn {
        action: IVaultRouter::VaultAction::try_from(record.kind)
            .map_err(|_| crate::errors::VaultRouterError::InvalidVaultAction)?,
        vault: record.vault,
        createdAt: record.created_at,
    })
}

pub(crate) fn apply_vault_action(storage: StorageHandle<'_>, proposal_id: U256) -> Result<()> {
    storage.with_checkpoint(|| {
        let record = state::vault_action(&storage, proposal_id)?;
        // Consume before any subcall. The checkpoint restores this on every failure.
        state::consume_vault_action(&storage, &record)?;
        let now = u128::from(storage.block_number()?);
        let deadline = u128::from(record.created_at)
            + u128::from(outbe_chain_constants::get_governance_voting_window_blocks());
        if now > deadline {
            return VaultRouterContract::new(storage.clone()).emit(
                IVaultRouter::VaultActionExpired {
                    proposalId: proposal_id,
                    vault: record.vault,
                },
            );
        }
        VaultRouterContract::new(storage.clone()).emit(IVaultRouter::VaultActionApplied {
            proposalId: proposal_id,
            vault: record.vault,
        })?;
        match record.kind {
            state::ADD_VAULT => super::add_vault(storage.clone(), record.vault),
            state::REMOVE_VAULT => super::remove_vault(storage.clone(), record.vault),
            _ => Err(crate::errors::VaultRouterError::InvalidVaultAction.into()),
        }
    })
}
