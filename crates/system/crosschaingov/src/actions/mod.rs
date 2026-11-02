//! Vote actions on our multi-chain contracts.
//!
//! An action only builds its owner calls ([`Calls`]). The vote target runs
//! them through the executor. A chain id is also the Hyperlane domain of that
//! chain.

pub(crate) mod common;
pub(crate) mod crosschain;
pub(crate) mod tokens;

use alloy_primitives::{Address, Bytes};
use outbe_hyperlanecontroller::RemoteCall;
use outbe_primitives::error::Result;
use outbe_vote::errors::VoteError;
use serde::Deserialize;

/// One action, as strict JSON with an `operation` field.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Action {
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
}

/// The owner calls of one action: on Outbe first, then one remote message.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Calls {
    pub local: Vec<RemoteCall>,
    pub remote: Option<(u32, Vec<RemoteCall>)>,
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
    /// Decodes and checks `payload`. `outbe` is the chain id of this chain.
    pub fn decode(payload: &[u8], outbe: u64) -> Result<Self> {
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
        };
        if !valid {
            return Err(VoteError::InvalidPayload.into());
        }
        Ok(action)
    }

    /// The owner calls of the action. `outbe` is the chain id of this chain.
    pub fn calls(&self, outbe: u32) -> Calls {
        match self {
            Self::UpgradeContract {
                chain_id,
                proxy,
                implementation,
                call_data,
            } => Calls::on(
                *chain_id,
                outbe,
                vec![common::upgrade(*proxy, *implementation, call_data.clone())],
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
                remote: Some((
                    *remote_chain_id,
                    crosschain::link_side(
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
                local: vec![tokens::add_peer(
                    *token_bridge,
                    *remote_chain_id,
                    *remote_token_bridge,
                )],
                remote: Some((
                    *remote_chain_id,
                    vec![tokens::add_peer(*remote_token_bridge, outbe, *token_bridge)],
                )),
            },
        }
    }
}

/// A zero-value owner call of `to`.
fn call(to: Address, data: Vec<u8>) -> RemoteCall {
    RemoteCall {
        to,
        value: alloy_primitives::U256::ZERO,
        data: data.into(),
    }
}
