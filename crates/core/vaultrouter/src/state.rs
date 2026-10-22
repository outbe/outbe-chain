//! Local pending-approval storage and indexes. No external calls.

use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::{
    errors::VaultRouterError,
    schema::{ApprovedVaultAction, VaultRouterContract},
};

pub(crate) const ADD_VAULT: u8 = 1;
pub(crate) const REMOVE_VAULT: u8 = 2;

/// Canonical tally order determines supersession, not proposal ID magnitude.
pub(crate) fn record_approved_vault_action(
    storage: &StorageHandle<'_>,
    proposal_id: U256,
    vault: Address,
    kind: u8,
    approved_at: u64,
) -> Result<()> {
    if proposal_id.is_zero() || vault.is_zero() || !matches!(kind, ADD_VAULT | REMOVE_VAULT) {
        return Err(VaultRouterError::InvalidApprovedVaultAction.into());
    }
    storage.with_checkpoint(|| {
        let contract = VaultRouterContract::new(storage.clone());
        if contract.approved_vault_actions.exists(proposal_id)? {
            return Err(VaultRouterError::InvalidApprovedVaultAction.into());
        }
        let previous = contract.pending_vault_proposals.read(&vault)?;
        if !previous.is_zero() {
            let record = contract.approved_vault_actions.get(previous)?;
            if record.as_ref().map(|record| record.vault) != Some(vault) {
                return Err(
                    VaultRouterError::ApprovedVaultActionIndexInconsistent(previous).into(),
                );
            }
            contract.approved_vault_actions.delete(previous)?;
        }
        contract
            .approved_vault_actions
            .create(&ApprovedVaultAction {
                id: proposal_id,
                vault,
                kind,
                approved_at,
            })?;
        contract.pending_vault_proposals.write(&vault, proposal_id)
    })
}

pub(crate) fn approved_vault_action(
    storage: &StorageHandle<'_>,
    id: U256,
) -> Result<ApprovedVaultAction> {
    let contract = VaultRouterContract::new(storage.clone());
    let record = contract
        .approved_vault_actions
        .get(id)?
        .ok_or(VaultRouterError::ApprovedVaultActionNotFound(id))?;
    if contract.pending_vault_proposals.read(&record.vault)? != id {
        return Err(VaultRouterError::ApprovedVaultActionIndexInconsistent(id).into());
    }
    if !matches!(record.kind, ADD_VAULT | REMOVE_VAULT) {
        return Err(VaultRouterError::InvalidApprovedVaultAction.into());
    }
    Ok(record)
}

pub(crate) fn consume_approved_vault_action(
    storage: &StorageHandle<'_>,
    record: &ApprovedVaultAction,
) -> Result<()> {
    let contract = VaultRouterContract::new(storage.clone());
    contract.approved_vault_actions.delete(record.id)?;
    contract.pending_vault_proposals.clear(&record.vault)
}
