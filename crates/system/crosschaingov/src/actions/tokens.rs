//! Actions on the token bridges (`ERC7786TokenBridge` in `contracts/tokens`).

use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::RemoteCall;
use serde::Deserialize;

use super::{is_remote, Calls, Operation};
use crate::sol_ext::ITokenBridge;

/// Adds each token bridge as the peer of the other.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkTokenBridges {
    pub remote_chain_id: u32,
    pub token_bridge: Address,
    pub remote_token_bridge: Address,
}

impl Operation for LinkTokenBridges {
    fn is_valid(&self, outbe: u64) -> bool {
        is_remote(self.remote_chain_id, outbe)
            && !self.token_bridge.is_zero()
            && !self.remote_token_bridge.is_zero()
    }

    fn calls(&self, outbe: u32) -> Calls {
        Calls {
            local: vec![add_peer(
                self.token_bridge,
                self.remote_chain_id,
                self.remote_token_bridge,
            )],
            remote: Some((
                self.remote_chain_id,
                vec![add_peer(self.remote_token_bridge, outbe, self.token_bridge)],
            )),
        }
    }
}

fn add_peer(token_bridge: Address, chain_id: u32, peer: Address) -> RemoteCall {
    RemoteCall::new(
        token_bridge,
        ITokenBridge::addPeerCall {
            domain: chain_id,
            peerBridge: peer,
        }
        .abi_encode(),
    )
}
