// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {LoopbackGatewayAdapter} from "src/adapters/LoopbackGatewayAdapter.sol";

/// @dev Deploys each contract behind an ERC1967 proxy, as the deploy scripts do.
library Proxies {
    function bridge(address owner, address gateway) internal returns (ERC7786Bridge) {
        bytes memory init = abi.encodeCall(ERC7786Bridge.initialize, (owner, gateway));
        return ERC7786Bridge(address(new ERC1967Proxy(address(new ERC7786Bridge()), init)));
    }

    function hyperlane(address mailbox, address owner) internal returns (HyperlaneGatewayAdapter) {
        bytes memory init = abi.encodeCall(HyperlaneGatewayAdapter.initialize, (owner));
        return HyperlaneGatewayAdapter(address(new ERC1967Proxy(address(new HyperlaneGatewayAdapter(mailbox)), init)));
    }

    function layerZero(address endpoint, address owner) internal returns (LayerZeroGatewayAdapter) {
        bytes memory init = abi.encodeCall(LayerZeroGatewayAdapter.initialize, (owner));
        return LayerZeroGatewayAdapter(address(new ERC1967Proxy(address(new LayerZeroGatewayAdapter(endpoint)), init)));
    }

    function loopback(address hub, address owner) internal returns (LoopbackGatewayAdapter) {
        bytes memory init = abi.encodeCall(LoopbackGatewayAdapter.initialize, (owner));
        return LoopbackGatewayAdapter(address(new ERC1967Proxy(address(new LoopbackGatewayAdapter(hub)), init)));
    }
}
