//! Owner calls on the token bridge (`ERC7786TokenBridge`).

use alloy_primitives::Address;
use alloy_sol_types::{sol, SolCall};
use outbe_hyperlanecontroller::RemoteCall;

use super::call;

sol! {
    /// Owner function of `contracts/tokens/src/ERC7786TokenBridge.sol`.
    interface ITokenBridge {
        function addPeer(uint32 domain, address peerBridge) external;
    }
}

/// `token_bridge.addPeer(chain_id, peer)`.
pub(super) fn add_peer(token_bridge: Address, chain_id: u32, peer: Address) -> RemoteCall {
    call(
        token_bridge,
        ITokenBridge::addPeerCall {
            domain: chain_id,
            peerBridge: peer,
        }
        .abi_encode(),
    )
}
