//! The one vote target of the owner address: strict JSON actions on our
//! multi-chain contracts and on the Hyperlane chains.
//!
//! An action on our contracts only builds its owner calls. The executor runs
//! them. A Hyperlane action calls the methods of `hyperlanecontroller`. A
//! chain id is also the Hyperlane domain of that chain.
//!
//! The vote tally runs where sub-calls are not available. So an approved
//! proposal only stores the hash of its payload. Permissionless
//! `applyApproved(proposalId, payload)` runs it later, with the owner address
//! as `msg.sender` on Outbe and its Interchain Account on a remote chain.

pub(crate) mod crosschain;
pub(crate) mod tokens;
pub(crate) mod upgrade;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use outbe_hyperlanecontroller::HyperlaneControllerContract;
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};
use serde::Deserialize;

use crate::errors::CrosschainGovError;
use crate::executor::Executor;
use crate::precompile::ICrosschainGov::{self, Call};
use crate::schema::CrosschainGovContract;

/// One action, as strict JSON with an `operation` field.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Action {
    /// Upgrades the UUPS proxy `proxy` on `chain_id` to `implementation`, and
    /// then runs `call_data` on it (`proxy.upgradeToAndCall`). Empty
    /// `call_data` is a plain upgrade. With the current implementation,
    /// `call_data` runs an owner function, for example a configuration
    /// change.
    UpgradeContract {
        chain_id: u32,
        proxy: Address,
        implementation: Address,
        call_data: Bytes,
    },
    /// Pauses or unpauses the bridge on `chain_id`.
    SetBridgePaused {
        chain_id: u32,
        bridge: Address,
        paused: bool,
    },
    /// Sets the adapter that the bridge on `chain_id` sends through to
    /// `destination_chain_id` (`bridge.setGateway`). A zero `adapter` clears
    /// it back to the default adapter.
    SetBridgeGateway {
        chain_id: u32,
        bridge: Address,
        destination_chain_id: u32,
        adapter: Address,
    },
    /// Links the Outbe bridge and the bridge on `remote_chain_id` both ways
    /// through their Hyperlane adapters: each adapter trusts the other, each
    /// bridge trusts the other, and each bridge sends to the other through
    /// its adapter.
    LinkBridges {
        remote_chain_id: u32,
        bridge: Address,
        adapter: Address,
        remote_bridge: Address,
        remote_adapter: Address,
    },
    /// Adds each token bridge as the peer of the other.
    LinkTokenBridges {
        remote_chain_id: u32,
        token_bridge: Address,
        remote_token_bridge: Address,
    },
    /// Puts `chain_id` under the validator sync: the Outbe ICA router enrolls
    /// `ica_router` (the ICA router on that chain), then the owner accepts
    /// the ownership of the ISM there and starts to sync the Outbe validators
    /// to it.
    AddHyperlaneChain {
        chain_id: u32,
        ica_router: Address,
        ism: Address,
        hook: Address,
    },
    /// Stops the validator sync to `chain_id`.
    RemoveHyperlaneChain { chain_id: u32 },
}

/// The owner calls of one operation: on Outbe first, then one message for
/// each remote domain.
#[derive(Default)]
struct Calls {
    local: Vec<Call>,
    remote: Vec<(u32, Vec<Call>)>,
}

impl Calls {
    /// All `calls` run on `chain_id`. `outbe` is the chain id of this chain.
    fn on(chain_id: u32, outbe: u32, calls: Vec<Call>) -> Self {
        if chain_id == outbe {
            Self {
                local: calls,
                remote: Vec::new(),
            }
        } else {
            Self {
                local: Vec::new(),
                remote: vec![(chain_id, calls)],
            }
        }
    }
}

impl Action {
    /// Decodes and checks `payload`. `outbe` is the chain id of this chain.
    fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
        // Decode the original bytes so duplicate fields are rejected.
        let action: Self =
            serde_json::from_slice(payload).map_err(|_| VoteError::InvalidPayload)?;
        let remote = |chain_id: &u32| *chain_id != 0 && u64::from(*chain_id) != outbe;
        let valid = match &action {
            Self::UpgradeContract {
                chain_id,
                proxy,
                implementation,
                ..
            } => *chain_id != 0 && !proxy.is_zero() && !implementation.is_zero(),
            Self::SetBridgePaused {
                chain_id, bridge, ..
            } => *chain_id != 0 && !bridge.is_zero(),
            Self::SetBridgeGateway {
                chain_id,
                bridge,
                destination_chain_id,
                ..
            } => {
                *chain_id != 0
                    && *destination_chain_id != 0
                    && destination_chain_id != chain_id
                    && !bridge.is_zero()
            }
            Self::LinkBridges {
                remote_chain_id,
                bridge,
                adapter,
                remote_bridge,
                remote_adapter,
            } => {
                remote(remote_chain_id)
                    && ![bridge, adapter, remote_bridge, remote_adapter]
                        .iter()
                        .any(|address| address.is_zero())
            }
            Self::LinkTokenBridges {
                remote_chain_id,
                token_bridge,
                remote_token_bridge,
            } => {
                remote(remote_chain_id) && !token_bridge.is_zero() && !remote_token_bridge.is_zero()
            }
            Self::AddHyperlaneChain {
                chain_id,
                ica_router,
                ism,
                hook,
            } => remote(chain_id) && ![ica_router, ism, hook].iter().any(|a| a.is_zero()),
            Self::RemoveHyperlaneChain { chain_id } => remote(chain_id),
        };
        if !valid {
            return Err(VoteError::InvalidPayload.into());
        }
        Ok(action)
    }

    /// Runs the action. Every owner call goes through `executor`.
    fn apply(
        &self,
        controller: &mut HyperlaneControllerContract<'_>,
        executor: &mut Executor<'_>,
    ) -> Result<()> {
        match self {
            Self::AddHyperlaneChain {
                chain_id,
                ica_router,
                ism,
                hook,
            } => {
                // The hub methods make their owner calls back through
                // `callLocal` and `callRemote` of this precompile.
                controller.enroll_ica_router(*chain_id, *ica_router)?;
                controller.add_domain(*chain_id, *ism, *hook)
            }
            Self::RemoveHyperlaneChain { chain_id } => controller.remove_domain(*chain_id),
            _ => {
                let calls = self.calls(controller.local_domain()?);
                for call in calls.local {
                    executor.call_local(call.to, call.value, call.data)?;
                }
                for (domain, remote) in &calls.remote {
                    executor.call_remote(*domain, remote)?;
                }
                Ok(())
            }
        }
    }

    /// The owner calls of an action on our contracts. `outbe` is the chain
    /// id of this chain. A Hyperlane action has none here.
    fn calls(&self, outbe: u32) -> Calls {
        match self {
            Self::UpgradeContract {
                chain_id,
                proxy,
                implementation,
                call_data,
            } => Calls::on(
                *chain_id,
                outbe,
                vec![upgrade::upgrade(*proxy, *implementation, call_data.clone())],
            ),
            Self::SetBridgePaused {
                chain_id,
                bridge,
                paused,
            } => Calls::on(
                *chain_id,
                outbe,
                vec![crosschain::set_paused(*bridge, *paused)],
            ),
            Self::SetBridgeGateway {
                chain_id,
                bridge,
                destination_chain_id,
                adapter,
            } => Calls::on(
                *chain_id,
                outbe,
                vec![crosschain::set_gateway(
                    *bridge,
                    *destination_chain_id,
                    *adapter,
                )],
            ),
            Self::LinkBridges {
                remote_chain_id,
                bridge,
                adapter,
                remote_bridge,
                remote_adapter,
            } => Calls {
                local: crosschain::link_side(
                    (*bridge, *adapter),
                    *remote_chain_id,
                    (*remote_bridge, *remote_adapter),
                ),
                remote: vec![(
                    *remote_chain_id,
                    crosschain::link_side(
                        (*remote_bridge, *remote_adapter),
                        outbe,
                        (*bridge, *adapter),
                    ),
                )],
            },
            Self::LinkTokenBridges {
                remote_chain_id,
                token_bridge,
                remote_token_bridge,
            } => Calls {
                local: vec![tokens::add_peer(
                    *token_bridge,
                    *remote_chain_id,
                    *remote_token_bridge,
                )],
                remote: vec![(
                    *remote_chain_id,
                    vec![tokens::add_peer(*remote_token_bridge, outbe, *token_bridge)],
                )],
            },
            Self::AddHyperlaneChain { .. } | Self::RemoveHyperlaneChain { .. } => Calls::default(),
        }
    }
}

/// A zero-value owner call of `to`.
fn call(to: Address, data: Vec<u8>) -> Call {
    Call {
        to,
        value: U256::ZERO,
        data: data.into(),
    }
}

impl CrosschainGovContract<'_> {
    /// Runs an approved action once, inside one checkpoint.
    pub fn apply_approved(&mut self, proposal_id: U256, payload: &[u8]) -> Result<()> {
        let approved = self.approved_payload.read(&proposal_id)?;
        if approved == B256::ZERO {
            return Err(CrosschainGovError::UnknownApprovedAction { proposal_id }.into());
        }
        if keccak256(payload) != approved {
            return Err(CrosschainGovError::ApprovedPayloadMismatch { proposal_id }.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            // Consume first. The checkpoint restores it if the action fails.
            self.approved_payload.write(&proposal_id, B256::ZERO)?;
            let deadline = self
                .approved_at
                .read(&proposal_id)?
                .saturating_add(outbe_chain_constants::get_governance_voting_window_blocks());
            if self.storage.block_number()? > deadline {
                return self.emit(ICrosschainGov::ApprovedActionExpired {
                    proposalId: proposal_id,
                });
            }
            let action = Action::decode(payload, self.storage.chain_id()?)
                .map_err(|_| VoteError::stored_payload_invalid("CrosschainGov"))?;
            let mut controller = HyperlaneControllerContract::new(self.storage.clone());
            let mut executor = Executor::new(self.storage.clone());
            action.apply(&mut controller, &mut executor)?;
            self.emit(ICrosschainGov::ApprovedActionApplied {
                proposalId: proposal_id,
            })
        })
    }
}

/// Validator-only, zero-value vote target. Approval stores the payload hash
/// for `applyApproved`.
pub struct CrosschainVoteTarget;

impl VoteTarget for CrosschainVoteTarget {
    fn target_module(&self) -> Address {
        CROSSCHAIN_GOV_ADDRESS
    }

    fn validate(&self, payload: &[u8], context: VoteTargetContext) -> Result<()> {
        decode_proposal_payload(payload)?;
        Action::decode(payload, context.chain_id).map(|_| ())
    }

    fn handle_approved(
        &self,
        ctx: &BlockRuntimeContext,
        proposal_id: U256,
        payload: &[u8],
        context: VoteTargetContext,
    ) -> Result<TargetExecutionOutcome> {
        // Stored-payload corruption is an invariant failure, never proposal Error.
        decode_stored_proposal_payload(payload, "CrosschainGov")?;
        Action::decode(payload, context.chain_id)
            .map_err(|_| VoteError::stored_payload_invalid("CrosschainGov"))?;
        let gov = CrosschainGovContract::new(ctx.storage.clone());
        gov.approved_payload
            .write(&proposal_id, keccak256(payload))?;
        gov.approved_at
            .write(&proposal_id, ctx.block.block_number)?;
        Ok(TargetExecutionOutcome::Applied)
    }
}
