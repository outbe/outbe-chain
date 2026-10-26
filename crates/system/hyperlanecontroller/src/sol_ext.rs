//! Outbound sub-call ABI surfaces.
//!
//! The Hyperlane contract interfaces that the controller calls through
//! `StorageHandle::call` / `StorageHandle::staticcall`. This is NOT the
//! precompile's own inbound ABI (`precompile::IHyperlaneController`).

use alloy_sol_types::sol;

sol!("../../../contracts/crosschain/src/interfaces/IHyperlaneCore.sol");

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

    /// Owner function of `contracts/tokens/src/ERC7786TokenBridge.sol`.
    interface ITokenBridge {
        function addPeer(uint32 domain, address peerBridge) external;
    }
}
