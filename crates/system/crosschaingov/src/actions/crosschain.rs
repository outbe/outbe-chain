//! Actions on the crosschain hub (`ERC7786Bridge` in `contracts/crosschain`)
//! and its gateway adapters. The hub is one per chain, between the token
//! bridges and the transport adapters.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::RemoteCall;
use serde::Deserialize;

use super::{is_remote, Calls, Operation};
use crate::sol_ext::{IERC7786Bridge, IHyperlaneGatewayAdapter};

/// Pauses or unpauses the bridge on `chain_id`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetBridgePaused {
    pub chain_id: u32,
    pub bridge: Address,
    pub paused: bool,
}

impl Operation for SetBridgePaused {
    fn is_valid(&self, _outbe: u64) -> bool {
        self.chain_id != 0 && !self.bridge.is_zero()
    }

    fn calls(&self, outbe: u32) -> Calls {
        let data = if self.paused {
            IERC7786Bridge::pauseCall {}.abi_encode()
        } else {
            IERC7786Bridge::unpauseCall {}.abi_encode()
        };
        Calls::on(
            self.chain_id,
            outbe,
            vec![RemoteCall::new(self.bridge, data)],
        )
    }
}

/// Sets the adapter that the bridge on `chain_id` sends through to
/// `destination_chain_id` (`bridge.setGateway`). A zero `adapter` clears it
/// back to the default adapter.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetBridgeGateway {
    pub chain_id: u32,
    pub bridge: Address,
    pub destination_chain_id: u32,
    pub adapter: Address,
}

impl Operation for SetBridgeGateway {
    fn is_valid(&self, _outbe: u64) -> bool {
        self.chain_id != 0
            && self.destination_chain_id != 0
            && self.destination_chain_id != self.chain_id
            && !self.bridge.is_zero()
    }

    fn calls(&self, outbe: u32) -> Calls {
        Calls::on(
            self.chain_id,
            outbe,
            vec![RemoteCall::new(
                self.bridge,
                set_gateway(self.destination_chain_id, self.adapter),
            )],
        )
    }
}

/// Links the Outbe bridge and the bridge on `remote_chain_id` both ways
/// through their Hyperlane adapters: each adapter trusts the other, each
/// bridge trusts the other, and each bridge sends to the other through its
/// adapter.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkBridges {
    pub remote_chain_id: u32,
    pub bridge: Address,
    pub adapter: Address,
    pub remote_bridge: Address,
    pub remote_adapter: Address,
}

impl Operation for LinkBridges {
    fn is_valid(&self, outbe: u64) -> bool {
        is_remote(self.remote_chain_id, outbe)
            && ![
                self.bridge,
                self.adapter,
                self.remote_bridge,
                self.remote_adapter,
            ]
            .iter()
            .any(|address| address.is_zero())
    }

    fn calls(&self, outbe: u32) -> Calls {
        let outbe_side = (self.bridge, self.adapter);
        let remote_side = (self.remote_bridge, self.remote_adapter);
        Calls {
            local: link_side(outbe_side, self.remote_chain_id, remote_side),
            remote: Some((
                self.remote_chain_id,
                link_side(remote_side, outbe, outbe_side),
            )),
        }
    }
}

fn set_gateway(chain_id: u32, gateway: Address) -> Vec<u8> {
    IERC7786Bridge::setGatewayCall {
        chainId: U256::from(chain_id),
        gateway,
    }
    .abi_encode()
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
        RemoteCall::new(adapter, trust),
        RemoteCall::new(bridge, register),
        RemoteCall::new(bridge, set_gateway(other_chain_id, adapter)),
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
