//! Local pending vault action storage, indexes and events. No external calls.

use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{
    errors::VaultRouterError,
    precompile::IVaultRouter,
    schema::{VaultActionData, VaultRouterContract},
};

pub(crate) const ADD_VAULT: u8 = 1;
pub(crate) const REMOVE_VAULT: u8 = 2;

/// Records the vault action of an approved proposal and emits `VaultActionCreated`.
/// A waiting action for the same vault is deleted first and emits `VaultActionCanceled`.
/// Canonical tally order determines supersession, not proposal ID magnitude.
pub(crate) fn record_vault_action(
    storage: &StorageHandle<'_>,
    proposal_id: U256,
    vault: Address,
    kind: u8,
    created_at: u64,
) -> Result<()> {
    if proposal_id.is_zero() || vault.is_zero() || !matches!(kind, ADD_VAULT | REMOVE_VAULT) {
        return Err(VaultRouterError::InvalidVaultAction.into());
    }
    let action_kind = IVaultRouter::VaultAction::try_from(kind)
        .map_err(|_| VaultRouterError::InvalidVaultAction)?;
    storage.with_checkpoint(|| {
        let mut contract = VaultRouterContract::new(storage.clone());
        if contract.vault_actions.exists(proposal_id)? {
            return Err(VaultRouterError::InvalidVaultAction.into());
        }
        let previous = contract.pending_vault_proposals.read(&vault)?;
        if !previous.is_zero() {
            let record = contract.vault_actions.get(previous)?;
            if record.as_ref().map(|record| record.vault) != Some(vault) {
                return Err(VaultRouterError::VaultActionIndexInconsistent(previous).into());
            }
            contract.vault_actions.delete(previous)?;
            contract.emit(IVaultRouter::VaultActionCanceled {
                proposalId: previous,
                vault,
                newProposalId: proposal_id,
            })?;
        }
        contract.vault_actions.create(&VaultActionData {
            id: proposal_id,
            vault,
            kind,
            created_at,
        })?;
        contract
            .pending_vault_proposals
            .write(&vault, proposal_id)?;
        contract.emit(IVaultRouter::VaultActionCreated {
            proposalId: proposal_id,
            vault,
            kind: action_kind,
        })
    })
}

pub(crate) fn vault_action(storage: &StorageHandle<'_>, id: U256) -> Result<VaultActionData> {
    let contract = VaultRouterContract::new(storage.clone());
    let record = contract
        .vault_actions
        .get(id)?
        .ok_or(VaultRouterError::VaultActionNotFound(id))?;
    if contract.pending_vault_proposals.read(&record.vault)? != id {
        return Err(VaultRouterError::VaultActionIndexInconsistent(id).into());
    }
    if !matches!(record.kind, ADD_VAULT | REMOVE_VAULT) {
        return Err(VaultRouterError::InvalidVaultAction.into());
    }
    Ok(record)
}

pub(crate) fn consume_vault_action(
    storage: &StorageHandle<'_>,
    record: &VaultActionData,
) -> Result<()> {
    let contract = VaultRouterContract::new(storage.clone());
    contract.vault_actions.delete(record.id)?;
    contract.pending_vault_proposals.clear(&record.vault)
}
