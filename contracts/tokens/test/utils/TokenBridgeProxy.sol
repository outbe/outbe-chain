// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";

/// @dev Deploys a token bridge behind an ERC1967 proxy, as the route script does. It takes two
///      creations from the caller: the implementation, then the proxy.
library TokenBridgeProxy {
    function deploy(
        address token,
        address hub,
        ERC7786TokenBridge.TokenBridgeMode mode,
        address owner,
        ERC7786TokenBridge.Peer[] memory peers
    ) internal returns (ERC7786TokenBridge) {
        address implementation = address(new ERC7786TokenBridge(token, hub, mode));
        bytes memory init = abi.encodeCall(ERC7786TokenBridge.initialize, (owner, peers));
        return ERC7786TokenBridge(address(new ERC1967Proxy(implementation, init)));
    }
}
