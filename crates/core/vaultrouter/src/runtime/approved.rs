//! Vault changes that a validator vote approved.
//!
//! `add_vault` and `remove_vault` make EVM sub-calls into the vault and its
//! asset. The vote tally runs in the begin-block hook, where sub-calls are not
//! available. So the vote target only records the approved action here. Then
//! anyone applies it with `applyApproved` in a normal transaction.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use super::{add_vault, remove_vault};
use crate::api::IVaultRouter;
use crate::api::IVaultRouter::VaultAction;
use crate::errors::VaultRouterError;
use crate::schema::{ApprovedVaultAction, VaultRouterContract};

/// Records the vault action that proposal `proposal_id` approved at block
/// `approved_at`. A pending action of an older proposal on the same vault is
/// deleted, so that only the latest vote on a vault can be applied.
pub(crate) fn record_approved_vault_action(
    storage: StorageHandle<'_>,
    proposal_id: U256,
    action: VaultAction,
    vault: Address,
    approved_at: u64,
) -> Result<()> {
    let mut contract = VaultRouterContract::new(storage);
    let superseded = contract.pending_vault_actions.read(&vault)?;
    if !superseded.is_zero() {
        contract.approved_vault_actions.delete(superseded)?;
        contract.emit(IVaultRouter::VaultActionSuperseded {
            proposalId: superseded,
            vault,
            supersededBy: proposal_id,
        })?;
    }
    contract
        .approved_vault_actions
        .create(&ApprovedVaultAction {
            proposal_id,
            action: action as u8,
            vault,
            approved_at,
        })?;
    contract.pending_vault_actions.write(&vault, proposal_id)?;
    contract.emit(IVaultRouter::VaultActionApproved {
        proposalId: proposal_id,
        vault,
        action,
    })
}

/// `applyApproved`: applies the pending vault action of `proposal_id` with all
/// the checks of `add_vault` or `remove_vault`, then deletes it. An expired
/// action is deleted without being applied, and the call does not revert.
pub(crate) fn apply_approved(storage: StorageHandle<'_>, proposal_id: U256) -> Result<()> {
    let mut contract = VaultRouterContract::new(storage.clone());
    let Some(record) = contract.approved_vault_actions.get(proposal_id)? else {
        return Err(VaultRouterError::ApprovedVaultActionNotFound(proposal_id).into());
    };
    let action = vault_action(record.action);
    let expires_after = record
        .approved_at
        .saturating_add(outbe_chain_constants::get_governance_voting_window_blocks());
    if storage.block_number()? > expires_after {
        delete_pending(&contract, &record)?;
        return contract.emit(IVaultRouter::VaultActionExpired {
            proposalId: proposal_id,
            vault: record.vault,
            action,
        });
    }
    match action {
        VaultAction::AddVault => add_vault(storage, record.vault)?,
        VaultAction::RemoveVault => remove_vault(storage, record.vault)?,
        _ => return Err(VaultRouterError::ApprovedVaultActionNotFound(proposal_id).into()),
    }
    delete_pending(&contract, &record)
}

/// `approvedVaultAction`: the pending action of `proposal_id`, or `Unknown`
/// with a zero vault if none is pending.
pub fn approved_vault_action(
    storage: &StorageHandle<'_>,
    proposal_id: U256,
) -> Result<IVaultRouter::approvedVaultActionReturn> {
    let contract = VaultRouterContract::new(storage.clone());
    let record = contract
        .approved_vault_actions
        .get(proposal_id)?
        .unwrap_or_default();
    Ok(IVaultRouter::approvedVaultActionReturn {
        action: vault_action(record.action),
        vault: record.vault,
        approvedAt: record.approved_at,
    })
}

fn vault_action(value: u8) -> VaultAction {
    VaultAction::try_from(value).unwrap_or(VaultAction::Unknown)
}

/// Deletes `record` and its vault index entry.
fn delete_pending(contract: &VaultRouterContract<'_>, record: &ApprovedVaultAction) -> Result<()> {
    contract.approved_vault_actions.delete(record.proposal_id)?;
    if contract.pending_vault_actions.read(&record.vault)? == record.proposal_id {
        contract.pending_vault_actions.clear(&record.vault)?;
    }
    Ok(())
}
