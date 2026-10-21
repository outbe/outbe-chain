//! Strict JSON governance actions for the ownerless VaultRouter.

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::stablecoin::validate_currency_code;
use outbe_vote::errors::VoteError;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};
use serde::Deserialize;

use crate::{api::IVaultRouter, runtime, state};

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
    AddLiquiditySource {
        address: Address,
        r#type: u8,
    },
    RemoveLiquiditySource {
        address: Address,
    },
    AddLiquidityTarget {
        address: Address,
        r#type: u8,
    },
    RemoveLiquidityTarget {
        address: Address,
    },
}

impl Action {
    fn decode(payload: &[u8]) -> Result<Self> {
        // Decode the original bytes as well as the shared JSON envelope so duplicate
        // fields cannot be hidden by Value's last-field-wins representation.
        let action: Self =
            serde_json::from_slice(payload).map_err(|_| VoteError::InvalidPayload)?;
        let address = match &action {
            Self::AddVault {
                vault,
                asset,
                iso_code,
            } => {
                if asset.is_zero() || validate_currency_code(*iso_code).is_err() {
                    return Err(VoteError::InvalidPayload.into());
                }
                vault
            }
            Self::RemoveVault { vault } => vault,
            Self::AddLiquiditySource { address, r#type } => {
                if *r#type == 0 || IVaultRouter::StablesSource::try_from(*r#type).is_err() {
                    return Err(VoteError::InvalidPayload.into());
                }
                address
            }
            Self::AddLiquidityTarget { address, r#type } => {
                if *r#type == 0 || IVaultRouter::StablesTarget::try_from(*r#type).is_err() {
                    return Err(VoteError::InvalidPayload.into());
                }
                address
            }
            Self::RemoveLiquiditySource { address } | Self::RemoveLiquidityTarget { address } => {
                address
            }
        };
        if address.is_zero() {
            return Err(VoteError::InvalidPayload.into());
        }
        Ok(action)
    }

    fn apply(&self, ctx: &BlockRuntimeContext<'_>, proposal_id: U256) -> Result<()> {
        let storage = ctx.storage.clone();
        match self {
            Self::AddVault { vault, .. } => state::record_approved_vault_action(
                &storage,
                proposal_id,
                *vault,
                state::ADD_VAULT,
                ctx.block.block_number,
            ),
            Self::RemoveVault { vault } => state::record_approved_vault_action(
                &storage,
                proposal_id,
                *vault,
                state::REMOVE_VAULT,
                ctx.block.block_number,
            ),
            Self::AddLiquiditySource { address, r#type } => {
                runtime::add_liquidity_source(storage, *address, *r#type)
            }
            Self::RemoveLiquiditySource { address } => {
                runtime::remove_liquidity_source(storage, *address)
            }
            Self::AddLiquidityTarget { address, r#type } => {
                runtime::add_liquidity_target(storage, *address, *r#type)
            }
            Self::RemoveLiquidityTarget { address } => {
                runtime::remove_liquidity_target(storage, *address)
            }
        }
    }
}

/// Validator-only, zero-value vote target; vault mutations are staged for applyApproved.
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
            .map_err(|_| VoteError::stored_payload_invalid("VaultRouter"))?;
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
