// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {LoopbackGatewayAdapter} from "src/adapters/LoopbackGatewayAdapter.sol";
import {DeployBase} from "./DeployBase.sol";

/// @dev Deploys the gateway adapters via CREATE3 (same address on every chain).
///      `run()` deploys each adapter only if its endpoint env is set:
///      - LayerZero when `LZ_ENDPOINT` is present.
///      - Hyperlane when `HYPERLANE_MAILBOX` is present.
///      - The loopback (same-chain) adapter when `WIRE_LOOPBACK` is true.
///
///      The deployer owns each adapter until 4_HandOver gives it to governance.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATE3_FACTORY_ADDRESS`, and at least one of
/// `LZ_ENDPOINT` / `HYPERLANE_MAILBOX`.
contract DeployAdapters is DeployBase {
    function run() public virtual {
        string memory salt = vm.envString("CONTRACT_SALT");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        address deployer = _deployer();

        address lzEndpoint = vm.envOr("LZ_ENDPOINT", address(0));
        address mailbox = vm.envOr("HYPERLANE_MAILBOX", address(0));
        require(lzEndpoint != address(0) || mailbox != address(0), "set LZ_ENDPOINT and/or HYPERLANE_MAILBOX");

        vm.startBroadcast(_pk());
        if (lzEndpoint != address(0)) {
            console2.log("LayerZeroGatewayAdapter:", deployLayerZeroAdapter(factory, salt, deployer, lzEndpoint));
        }
        if (mailbox != address(0)) {
            console2.log("HyperlaneGatewayAdapter:", deployHyperlaneAdapter(factory, salt, deployer, mailbox));
        }
        if (vm.envOr("WIRE_LOOPBACK", false)) {
            console2.log("LoopbackGatewayAdapter:", deployLoopbackAdapter(factory, salt, deployer, deployer));
        }
        vm.stopBroadcast();
    }

    function deployLayerZeroAdapter(address factory, string memory salt, address owner, address lzEndpoint)
        public
        returns (address)
    {
        bytes32 saltHash = keccak256(abi.encodePacked("LayerZeroGatewayAdapter", salt));
        bytes memory code = abi.encodePacked(type(LayerZeroGatewayAdapter).creationCode, abi.encode(lzEndpoint, owner));
        return Create3Factory(factory).deploy(saltHash, code);
    }

    function deployHyperlaneAdapter(address factory, string memory salt, address owner, address mailbox)
        public
        returns (address)
    {
        bytes32 saltHash = keccak256(abi.encodePacked("HyperlaneGatewayAdapter", salt));
        bytes memory code = abi.encodePacked(type(HyperlaneGatewayAdapter).creationCode, abi.encode(mailbox, owner));
        return Create3Factory(factory).deploy(saltHash, code);
    }

    function deployLoopbackAdapter(address factory, string memory salt, address deployer, address owner)
        public
        returns (address)
    {
        // The hub address is CREATE3-deterministic, so the script can deploy the adapter before the
        // bridge itself.
        address hub = Create3Factory(factory).predict(deployer, keccak256(abi.encodePacked("ERC7786Bridge", salt)));
        bytes32 saltHash = keccak256(abi.encodePacked("LoopbackGatewayAdapter", salt));
        bytes memory code = abi.encodePacked(type(LoopbackGatewayAdapter).creationCode, abi.encode(hub, owner));
        return Create3Factory(factory).deploy(saltHash, code);
    }
}
