//! Owner calls on the ERC-7786 hub (`ERC7786Bridge`) and its Hyperlane adapter.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_hyperlanecontroller::RemoteCall;

use super::call;

sol! {
    /// Owner functions of `contracts/crosschain/src/ERC7786Bridge.sol`.
    interface IERC7786Bridge {
        function pause() external;
        function unpause() external;
        function setGateway(uint256 chainId, address gateway) external;
        function registerRemoteBridge(bytes calldata bridge) external;
    }

    /// Owner function of `contracts/crosschain/src/adapters/HyperlaneGatewayAdapter.sol`.
    interface IHyperlaneGatewayAdapter {
        function setRouterWithChain(uint32 domain, bytes32 router, uint256 chainId) external;
    }
}

/// `bridge.pause()` or `bridge.unpause()`.
pub(super) fn set_paused(bridge: Address, paused: bool) -> RemoteCall {
    let data = if paused {
        IERC7786Bridge::pauseCall {}.abi_encode()
    } else {
        IERC7786Bridge::unpauseCall {}.abi_encode()
    };
    call(bridge, data)
}

/// `bridge.setGateway(chain_id, gateway)`.
pub(super) fn set_gateway(bridge: Address, chain_id: u32, gateway: Address) -> RemoteCall {
    call(
        bridge,
        IERC7786Bridge::setGatewayCall {
            chainId: U256::from(chain_id),
            gateway,
        }
        .abi_encode(),
    )
}

/// The three owner calls on one side of a bridge link to `other_chain_id`.
pub(super) fn link_side(
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
        set_gateway(bridge, other_chain_id, adapter),
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
