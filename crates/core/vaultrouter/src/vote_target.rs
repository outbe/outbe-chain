//! Strict JSON governance actions for the ownerless VaultRouter.

use alloy_primitives::{Address, U256};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;
use outbe_system_common::stablecoin::validate_currency_code;
use outbe_system_common::vote_target::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext, VoteTargetError,
};
use serde::Deserialize;

use crate::{precompile::IVaultRouter, runtime, state};

#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Action {
    AddVault {
        vault: Address,
        asset: Address,
        iso_code: u16,
    },
    RemoveVault {
        vault: Address,
    },
    AddStablecoinSource {
        address: Address,
        r#type: u8,
    },
    RemoveStablecoinSource {
        address: Address,
    },
    AddStablecoinTarget {
        address: Address,
        r#type: u8,
    },
    RemoveStablecoinTarget {
        address: Address,
    },
}

impl Action {
    fn decode(payload: &[u8]) -> Result<Self> {
        // Decode the original bytes as well as the shared JSON envelope so duplicate
        // fields cannot be hidden by Value's last-field-wins representation.
        let action: Self =
            serde_json::from_slice(payload).map_err(|_| VoteTargetError::InvalidPayload)?;
        let address = match &action {
            Self::AddVault {
                vault,
                asset,
                iso_code,
            } => {
                if asset.is_zero() || validate_currency_code(*iso_code).is_err() {
                    return Err(VoteTargetError::InvalidPayload.into());
                }
                vault
            }
            Self::RemoveVault { vault } => vault,
            Self::AddStablecoinSource { address, r#type } => {
                if *r#type == 0 || IVaultRouter::StablecoinSource::try_from(*r#type).is_err() {
                    return Err(VoteTargetError::InvalidPayload.into());
                }
                address
            }
            Self::AddStablecoinTarget { address, r#type } => {
                if *r#type == 0 || IVaultRouter::StablecoinTarget::try_from(*r#type).is_err() {
                    return Err(VoteTargetError::InvalidPayload.into());
                }
                address
            }
            Self::RemoveStablecoinSource { address } | Self::RemoveStablecoinTarget { address } => {
                address
            }
        };
        if address.is_zero() {
            return Err(VoteTargetError::InvalidPayload.into());
        }
        Ok(action)
    }

    fn apply(&self, ctx: &BlockRuntimeContext<'_>, proposal_id: U256) -> Result<()> {
        let storage = ctx.storage.clone();
        match self {
            Self::AddVault { vault, .. } => state::record_vault_action(
                &storage,
                proposal_id,
                *vault,
                state::ADD_VAULT,
                ctx.block.block_number,
            ),
            Self::RemoveVault { vault } => state::record_vault_action(
                &storage,
                proposal_id,
                *vault,
                state::REMOVE_VAULT,
                ctx.block.block_number,
            ),
            Self::AddStablecoinSource { address, r#type } => {
                runtime::add_stablecoin_source(storage, *address, *r#type)
            }
            Self::RemoveStablecoinSource { address } => {
                runtime::remove_stablecoin_source(storage, *address)
            }
            Self::AddStablecoinTarget { address, r#type } => {
                runtime::add_stablecoin_target(storage, *address, *r#type)
            }
            Self::RemoveStablecoinTarget { address } => {
                runtime::remove_stablecoin_target(storage, *address)
            }
        }
    }
}

/// Validator-only, zero-value vote target; vault mutations are staged for applyVaultAction.
pub struct VaultRouterVoteTarget;

impl VoteTarget for VaultRouterVoteTarget {
    fn target_module(&self) -> Address {
        VAULT_ROUTER_ADDRESS
    }

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        decode_proposal_payload(payload)?;
        Action::decode(payload).map(|_| ())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        // Stored-payload corruption is an invariant failure, never proposal Error.
        decode_stored_proposal_payload(payload, "VaultRouter")?;
        let action = Action::decode(payload)
            .map_err(|_| VoteTargetError::stored_payload_invalid("VaultRouter"))?;
        let result = ctx
            .storage
            .with_checkpoint(|| action.apply(ctx, proposal_id));
        match result {
            Ok(()) => Ok(TargetExecutionOutcome::Applied),
            Err(error @ (PrecompileError::Revert(_) | PrecompileError::RevertBytes(_))) => {
                Ok(TargetExecutionOutcome::Error {
                    reason: error.to_string(),
                })
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests;
