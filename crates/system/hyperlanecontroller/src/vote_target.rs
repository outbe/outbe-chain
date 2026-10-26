//! Strict JSON governance actions for the crosschain contracts that the
//! controller owns: the ERC-7786 bridge, its gateway adapters, the token
//! bridges and the Hyperlane validator domains (OIP-00354).
//!
//! The vote tally runs where sub-calls are not available. So an approved
//! proposal only stores the hash of its payload. Permissionless
//! `applyApproved(proposalId, payload)` runs it later, with the controller as
//! `msg.sender` on Outbe and its Interchain Account on a remote chain.
//!
//! A chain id is also the Hyperlane domain of that chain.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use outbe_vote::handlers::{
    decode_proposal_payload, decode_stored_proposal_payload, TargetExecutionOutcome, VoteTarget,
    VoteTargetContext,
};
use serde::Deserialize;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IERC7786Bridge, IHyperlaneGatewayAdapter, IHyperlaneRouter, ITokenBridge};
use crate::RemoteCall;

#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Action {
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
    /// Puts `chain_id` under the controller: the Outbe ICA router enrolls
    /// `ica_router` (the ICA router on that chain), then the controller
    /// accepts the ownership of the ISM there and starts to sync the Outbe
    /// validators to it.
    AddHyperlaneChain {
        chain_id: u32,
        ica_router: Address,
        ism: Address,
        hook: Address,
    },
    /// Stops the validator sync to `chain_id`.
    RemoveHyperlaneChain { chain_id: u32 },
}

/// The owner calls of one action: on Outbe first, then one remote message.
#[derive(Default)]
struct Calls {
    local: Vec<RemoteCall>,
    remote: Option<(u32, Vec<RemoteCall>)>,
}

impl Calls {
    /// All `calls` run on `chain_id`.
    fn on(chain_id: u32, outbe: u32, calls: Vec<RemoteCall>) -> Self {
        if chain_id == outbe {
            Self {
                local: calls,
                remote: None,
            }
        } else {
            Self {
                local: Vec::new(),
                remote: Some((chain_id, calls)),
            }
        }
    }
}

impl Action {
    fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
        // Decode the original bytes so duplicate fields are rejected.
        let action: Self =
            serde_json::from_slice(payload).map_err(|_| VoteError::InvalidPayload)?;
        let remote = |chain_id: &u32| *chain_id != 0 && u64::from(*chain_id) != outbe;
        let valid = match &action {
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

    fn calls(&self, outbe: u32) -> Calls {
        match self {
            Self::SetBridgePaused {
                chain_id,
                bridge,
                paused,
            } => {
                let data = if *paused {
                    IERC7786Bridge::pauseCall {}.abi_encode()
                } else {
                    IERC7786Bridge::unpauseCall {}.abi_encode()
                };
                Calls::on(*chain_id, outbe, vec![call(*bridge, data)])
            }
            Self::SetBridgeGateway {
                chain_id,
                bridge,
                destination_chain_id,
                adapter,
            } => Calls::on(
                *chain_id,
                outbe,
                vec![call(*bridge, set_gateway(*destination_chain_id, *adapter))],
            ),
            Self::LinkBridges {
                remote_chain_id,
                bridge,
                adapter,
                remote_bridge,
                remote_adapter,
            } => Calls {
                local: link_side(
                    (*bridge, *adapter),
                    *remote_chain_id,
                    (*remote_bridge, *remote_adapter),
                ),
                remote: Some((
                    *remote_chain_id,
                    link_side(
                        (*remote_bridge, *remote_adapter),
                        outbe,
                        (*bridge, *adapter),
                    ),
                )),
            },
            Self::LinkTokenBridges {
                remote_chain_id,
                token_bridge,
                remote_token_bridge,
            } => Calls {
                local: vec![add_peer(
                    *token_bridge,
                    *remote_chain_id,
                    *remote_token_bridge,
                )],
                remote: Some((
                    *remote_chain_id,
                    vec![add_peer(*remote_token_bridge, outbe, *token_bridge)],
                )),
            },
            Self::AddHyperlaneChain { .. } | Self::RemoveHyperlaneChain { .. } => Calls::default(),
        }
    }

    fn apply(&self, controller: &mut HyperlaneControllerContract<'_>) -> Result<()> {
        match self {
            Self::AddHyperlaneChain {
                chain_id,
                ica_router,
                ism,
                hook,
            } => {
                controller.enroll_ica_router(*chain_id, *ica_router)?;
                controller.add_domain(*chain_id, *ism, *hook)
            }
            Self::RemoveHyperlaneChain { chain_id } => controller.remove_domain(*chain_id),
            _ => {
                let calls = self.calls(controller.local_domain()?);
                for call in calls.local {
                    controller.call_local(call.to, call.value, call.data)?;
                }
                if let Some((chain_id, remote)) = calls.remote {
                    controller.call_remote(chain_id, &remote)?;
                }
                Ok(())
            }
        }
    }
}

fn call(to: Address, data: Vec<u8>) -> RemoteCall {
    RemoteCall {
        to,
        value: U256::ZERO,
        data: data.into(),
    }
}

fn set_gateway(chain_id: u32, gateway: Address) -> Vec<u8> {
    IERC7786Bridge::setGatewayCall {
        chainId: U256::from(chain_id),
        gateway,
    }
    .abi_encode()
}

fn add_peer(token_bridge: Address, chain_id: u32, peer: Address) -> RemoteCall {
    call(
        token_bridge,
        ITokenBridge::addPeerCall {
            domain: chain_id,
            peerBridge: peer,
        }
        .abi_encode(),
    )
}

/// The three owner calls on one side of a bridge link to `other_chain_id`.
fn link_side(
    (bridge, adapter): (Address, Address),
    other_chain_id: u32,
    (other_bridge, other_adapter): (Address, Address),
) -> Vec<RemoteCall> {
    let trust = IHyperlaneGatewayAdapter::setRouterWithChainCall {
        domain: other_chain_id,
        router: other_adapter.into_word(),
        chainId: U256::from(other_chain_id),
    }
    .abi_encode();
    let register = IERC7786Bridge::registerRemoteBridgeCall {
        bridge: erc7930_evm(other_chain_id, other_bridge),
    }
    .abi_encode();
    vec![
        call(adapter, trust),
        call(bridge, register),
        call(bridge, set_gateway(other_chain_id, adapter)),
    ]
}

/// ERC-7930 v1 EVM address, as OpenZeppelin `InteroperableAddress.formatEvmV1`:
/// version, chain type 0x0000, the chain id without leading zero bytes, the address.
fn erc7930_evm(chain_id: u32, addr: Address) -> Bytes {
    let id = chain_id.to_be_bytes();
    let reference = &id[id.iter().position(|b| *b != 0).unwrap_or(id.len() - 1)..];
    let mut out = vec![0x00, 0x01, 0x00, 0x00, reference.len() as u8];
    out.extend_from_slice(reference);
    out.push(20);
    out.extend_from_slice(addr.as_slice());
    out.into()
}

impl HyperlaneControllerContract<'_> {
    /// Enrolls the ICA router of `domain` on the Outbe ICA router, so that
    /// `call_remote` can reach that chain. The router keeps the first
    /// enrollment forever, so the same router is a no-op and another router
    /// reverts.
    fn enroll_ica_router(&mut self, domain: u32, router: Address) -> Result<()> {
        let ica_router = self.ica_router.read()?;
        let ret = self.storage.staticcall(
            ica_router,
            IHyperlaneRouter::routersCall { _domain: domain }
                .abi_encode()
                .into(),
        )?;
        let enrolled = IHyperlaneRouter::routersCall::abi_decode_returns(&ret).map_err(|_| {
            HyperlaneControllerError::UndecodableReturn("InterchainAccountRouter routers")
        })?;
        if enrolled == router.into_word() {
            return Ok(());
        }
        if enrolled != B256::ZERO {
            return Err(HyperlaneControllerError::RemoteRouterMismatch { domain, enrolled }.into());
        }
        let data = IHyperlaneRouter::enrollRemoteRouterCall {
            _domain: domain,
            _router: router.into_word(),
        }
        .abi_encode();
        self.call_local(ica_router, U256::ZERO, data.into())
            .map(|_| ())
    }

    /// Runs an approved action once, inside one checkpoint.
    pub fn apply_approved(&mut self, proposal_id: U256, payload: &[u8]) -> Result<()> {
        let approved = self.approved_payload.read(&proposal_id)?;
        if approved == B256::ZERO {
            return Err(HyperlaneControllerError::UnknownApprovedAction { proposal_id }.into());
        }
        if keccak256(payload) != approved {
            return Err(HyperlaneControllerError::ApprovedPayloadMismatch { proposal_id }.into());
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
                return self.emit(IHyperlaneController::ApprovedActionExpired {
                    proposalId: proposal_id,
                });
            }
            let action = Action::decode(payload, self.storage.chain_id()?)
                .map_err(|_| VoteError::stored_payload_invalid("HyperlaneController"))?;
            action.apply(self)?;
            self.emit(IHyperlaneController::ApprovedActionApplied {
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
        HYPERLANE_CONTROLLER_ADDRESS
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
        decode_stored_proposal_payload(payload, "HyperlaneController")?;
        Action::decode(payload, context.chain_id)
            .map_err(|_| VoteError::stored_payload_invalid("HyperlaneController"))?;
        let controller = HyperlaneControllerContract::new(ctx.storage.clone());
        controller
            .approved_payload
            .write(&proposal_id, keccak256(payload))?;
        controller
            .approved_at
            .write(&proposal_id, ctx.block.block_number)?;
        Ok(TargetExecutionOutcome::Applied)
    }
}
