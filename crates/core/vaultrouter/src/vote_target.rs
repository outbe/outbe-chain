//! Validator-vote target for VaultRouter vault and route changes.
//!
//! One proposal carries exactly one action as strict JSON:
//!
//! ```text
//! {"action":"addVault","vault":"0x..","asset":"0x..","isoCode":840}
//! {"action":"removeVault","vault":"0x.."}
//! {"action":"addLiquiditySource","address":"0x..","type":1}
//! {"action":"removeLiquiditySource","address":"0x.."}
//! {"action":"addLiquidityTarget","address":"0x..","type":1}
//! {"action":"removeLiquidityTarget","address":"0x.."}
//! ```
//!
//! `type` is the `IVaultRouter.StablesSource` or `IVaultRouter.StablesTarget`
//! value. The `asset` and `isoCode` of `addVault` let validators examine the vault
//! before they vote. Execution does not use them: `applyApproved` reads the
//! asset and the ISO code from the vault on chain.
//!
//! An approved liquidity action applies in the vote tally. An approved vault
//! action only records a pending action, because `add_vault` and `remove_vault`
//! need EVM sub-calls. `applyApproved` applies it later.

use std::str::FromStr;

use alloy_primitives::{Address, U256};
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::stablecoin::validate_currency_code;
use outbe_primitives::storage::StorageHandle;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};
use serde::Deserialize;

use crate::api::IVaultRouter::{StablesSource, StablesTarget, VaultAction};
use crate::errors::VaultRouterError;
use crate::runtime;

#[derive(Debug, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum VaultRouterVotePayloadJson {
    AddVault {
        vault: String,
        asset: String,
        iso_code: u16,
    },
    RemoveVault {
        vault: String,
    },
    AddLiquiditySource {
        address: String,
        #[serde(rename = "type")]
        kind: u8,
    },
    RemoveLiquiditySource {
        address: String,
    },
    AddLiquidityTarget {
        address: String,
        #[serde(rename = "type")]
        kind: u8,
    },
    RemoveLiquidityTarget {
        address: String,
    },
}

/// Typed form of the JSON payload of a VaultRouter vote proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultRouterVoteAction {
    AddVault {
        vault: Address,
        asset: Address,
        iso_code: u16,
    },
    RemoveVault {
        vault: Address,
    },
    /// `source_type` is a known `IVaultRouter.StablesSource` other than `Unknown`.
    AddLiquiditySource {
        source: Address,
        source_type: u8,
    },
    RemoveLiquiditySource {
        source: Address,
    },
    /// `target_type` is a known `IVaultRouter.StablesTarget` other than `Unknown`.
    AddLiquidityTarget {
        target: Address,
        target_type: u8,
    },
    RemoveLiquidityTarget {
        target: Address,
    },
}

impl VaultRouterVoteAction {
    /// Decodes and validates a proposal payload. Reverts malformed JSON, unknown
    /// actions or fields, zero addresses, unknown types and invalid ISO codes.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        Self::from_json(&decode_proposal_payload(payload)?)
    }

    /// Validates the decoded JSON of a proposal payload.
    pub fn from_json(value: &serde_json::Value) -> Result<Self> {
        let decoded = VaultRouterVotePayloadJson::deserialize(value)
            .map_err(|_| VaultRouterError::InvalidProposalPayload)?;
        let action = match decoded {
            VaultRouterVotePayloadJson::AddVault {
                vault,
                asset,
                iso_code,
            } => {
                validate_currency_code(iso_code)?;
                Self::AddVault {
                    vault: parse_address(&vault)?,
                    asset: parse_address(&asset)?,
                    iso_code,
                }
            }
            VaultRouterVotePayloadJson::RemoveVault { vault } => Self::RemoveVault {
                vault: parse_address(&vault)?,
            },
            VaultRouterVotePayloadJson::AddLiquiditySource { address, kind } => {
                Self::AddLiquiditySource {
                    source: parse_address(&address)?,
                    source_type: match StablesSource::try_from(kind) {
                        Ok(StablesSource::Unknown) | Err(_) => {
                            return Err(VaultRouterError::InvalidLiquiditySource.into())
                        }
                        Ok(_) => kind,
                    },
                }
            }
            VaultRouterVotePayloadJson::RemoveLiquiditySource { address } => {
                Self::RemoveLiquiditySource {
                    source: parse_address(&address)?,
                }
            }
            VaultRouterVotePayloadJson::AddLiquidityTarget { address, kind } => {
                Self::AddLiquidityTarget {
                    target: parse_address(&address)?,
                    target_type: match StablesTarget::try_from(kind) {
                        Ok(StablesTarget::Unknown) | Err(_) => {
                            return Err(VaultRouterError::InvalidLiquidityTarget.into())
                        }
                        Ok(_) => kind,
                    },
                }
            }
            VaultRouterVotePayloadJson::RemoveLiquidityTarget { address } => {
                Self::RemoveLiquidityTarget {
                    target: parse_address(&address)?,
                }
            }
        };
        Ok(action)
    }

    /// The canonical JSON payload of this action.
    pub fn to_json(&self) -> String {
        let value = match *self {
            Self::AddVault {
                vault,
                asset,
                iso_code,
            } => serde_json::json!({
                "action": "addVault",
                "vault": format!("{vault:#x}"),
                "asset": format!("{asset:#x}"),
                "isoCode": iso_code,
            }),
            Self::RemoveVault { vault } => serde_json::json!({
                "action": "removeVault",
                "vault": format!("{vault:#x}"),
            }),
            Self::AddLiquiditySource {
                source,
                source_type,
            } => serde_json::json!({
                "action": "addLiquiditySource",
                "address": format!("{source:#x}"),
                "type": source_type,
            }),
            Self::RemoveLiquiditySource { source } => serde_json::json!({
                "action": "removeLiquiditySource",
                "address": format!("{source:#x}"),
            }),
            Self::AddLiquidityTarget {
                target,
                target_type,
            } => serde_json::json!({
                "action": "addLiquidityTarget",
                "address": format!("{target:#x}"),
                "type": target_type,
            }),
            Self::RemoveLiquidityTarget { target } => serde_json::json!({
                "action": "removeLiquidityTarget",
                "address": format!("{target:#x}"),
            }),
        };
        value.to_string()
    }

    /// Applies the action that proposal `proposal_id` approved at block
    /// `approved_at`. Vault actions become pending actions for `applyApproved`.
    fn apply_approved(
        self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
        approved_at: u64,
    ) -> Result<()> {
        match self {
            Self::AddVault { vault, .. } => runtime::record_approved_vault_action(
                storage,
                proposal_id,
                VaultAction::AddVault,
                vault,
                approved_at,
            ),
            Self::RemoveVault { vault } => runtime::record_approved_vault_action(
                storage,
                proposal_id,
                VaultAction::RemoveVault,
                vault,
                approved_at,
            ),
            Self::AddLiquiditySource {
                source,
                source_type,
            } => runtime::add_liquidity_source(storage, source, source_type),
            Self::RemoveLiquiditySource { source } => {
                runtime::remove_liquidity_source(storage, source)
            }
            Self::AddLiquidityTarget {
                target,
                target_type,
            } => runtime::add_liquidity_target(storage, target, target_type),
            Self::RemoveLiquidityTarget { target } => {
                runtime::remove_liquidity_target(storage, target)
            }
        }
    }
}

/// Parses a non-zero address.
fn parse_address(value: &str) -> Result<Address> {
    let address = Address::from_str(value).map_err(|_| VaultRouterError::InvalidProposalPayload)?;
    if address.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }
    Ok(address)
}

/// Compile-time Vote adapter owned by the VaultRouter. It keeps the default
/// admission (active validators only) and the default electorate (active set).
pub struct VaultRouterVoteTarget;

impl VoteTarget for VaultRouterVoteTarget {
    fn target_module(&self) -> Address {
        VAULT_ROUTER_ADDRESS
    }

    fn validate(&self, payload: &[u8], _context: VoteTargetContext) -> Result<()> {
        VaultRouterVoteAction::decode(payload).map(|_| ())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        _context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        let payload = decode_stored_proposal_payload(payload, "VaultRouter")?;
        let applied = VaultRouterVoteAction::from_json(&payload).and_then(|action| {
            ctx.with_checkpoint(|| {
                action.apply_approved(ctx.storage.clone(), proposal_id, ctx.block.block_number)
            })
        });
        match applied {
            Ok(()) => Ok(TargetExecutionOutcome::Applied),
            Err(PrecompileError::Revert(reason)) => Ok(TargetExecutionOutcome::Error { reason }),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests;
