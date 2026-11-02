//! Outbound sub-call ABI surfaces of the contracts that CrosschainGov owns.

use alloy_sol_types::sol;

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

    /// OpenZeppelin `UUPSUpgradeable`. Every multi-chain proxy authorizes it
    /// with `onlyOwner`.
    interface IUUPSUpgradeable {
        function upgradeToAndCall(address newImplementation, bytes calldata data) external payable;
    }
}
