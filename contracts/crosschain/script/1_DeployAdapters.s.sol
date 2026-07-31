// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {CreateX} from "./0_DeployCreateX.s.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {LoopbackGatewayAdapter} from "src/adapters/LoopbackGatewayAdapter.sol";

/// @dev Deploys the gateway adapters via CREATE3 (same address on every chain). `run()` deploys each adapter only if
///      its endpoint env is set: LayerZero when `LZ_ENDPOINT` is present, Hyperlane when `HYPERLANE_MAILBOX` is present,
///      the loopback (same-chain) adapter when `WIRE_LOOPBACK` is true.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATEX_ADDRESS`, `BRIDGE_OWNER`, and at least one of
/// `LZ_ENDPOINT` / `HYPERLANE_MAILBOX`.
contract DeployAdapters is Script {
    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address createX = vm.envAddress("CREATEX_ADDRESS");
        address owner = vm.envAddress("BRIDGE_OWNER");
        address deployer = vm.addr(deployerPk);

        address lzEndpoint = vm.envOr("LZ_ENDPOINT", address(0));
        address mailbox = vm.envOr("HYPERLANE_MAILBOX", address(0));
        require(lzEndpoint != address(0) || mailbox != address(0), "set LZ_ENDPOINT and/or HYPERLANE_MAILBOX");

        vm.startBroadcast(deployerPk);
        if (lzEndpoint != address(0)) {
            console2.log("LayerZeroGatewayAdapter:", deployLayerZeroAdapter(createX, salt, deployer, owner, lzEndpoint));
        }
        if (mailbox != address(0)) {
            console2.log("HyperlaneGatewayAdapter:", deployHyperlaneAdapter(createX, salt, deployer, owner, mailbox));
        }
        if (vm.envOr("WIRE_LOOPBACK", false)) {
            console2.log("LoopbackGatewayAdapter:", deployLoopbackAdapter(createX, salt, deployer, owner));
        }
        vm.stopBroadcast();
    }

    function deployLayerZeroAdapter(
        address createX,
        string memory salt,
        address deployer,
        address owner,
        address lzEndpoint
    ) public returns (address) {
        bytes32 saltHash = keccak256(abi.encodePacked("LayerZeroGatewayAdapter", salt, deployer));
        bytes memory code = abi.encodePacked(type(LayerZeroGatewayAdapter).creationCode, abi.encode(lzEndpoint, owner));
        return CreateX(createX).deployCreate3(saltHash, code);
    }

    function deployHyperlaneAdapter(
        address createX,
        string memory salt,
        address deployer,
        address owner,
        address mailbox
    ) public returns (address) {
        bytes32 saltHash = keccak256(abi.encodePacked("HyperlaneGatewayAdapter", salt, deployer));
        bytes memory code = abi.encodePacked(type(HyperlaneGatewayAdapter).creationCode, abi.encode(mailbox, owner));
        return CreateX(createX).deployCreate3(saltHash, code);
    }

    function deployLoopbackAdapter(address createX, string memory salt, address deployer, address owner)
        public
        returns (address)
    {
        // The hub address is CREATE3-deterministic, so the adapter can be deployed before the bridge itself.
        address hub =
            CreateX(createX).computeCreate3Address(keccak256(abi.encodePacked("ERC7786Bridge", salt, deployer)));
        bytes32 saltHash = keccak256(abi.encodePacked("LoopbackGatewayAdapter", salt, deployer));
        bytes memory code = abi.encodePacked(type(LoopbackGatewayAdapter).creationCode, abi.encode(hub, owner));
        return CreateX(createX).deployCreate3(saltHash, code);
    }
}
