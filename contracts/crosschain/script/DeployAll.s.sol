// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";

import {DeployCreateXDeterministic} from "./0_DeployCreateX.s.sol";
import {DeployAdapters} from "./1_DeployAdapters.s.sol";
import {DeployBridge} from "./2_DeployBridge.s.sol";
import {ConfigureBridge} from "./3_ConfigureBridge.s.sol";

/// @dev Full hub deploy + wiring on one chain, in one shot:
///   1. CreateX factory (reused if `CREATEX_ADDRESS` set)
///   2. Adapters — each deployed only when its endpoint env is set (`LZ_ENDPOINT` / `HYPERLANE_MAILBOX`)
///   3. Bridge (with the active gateway)
///   4. Wire remotes for each `REMOTE_CHAIN_IDS` (remote addresses == local CREATE3 addresses)
/// Remote addresses are deterministic, so step 4 is safe even before other chains are deployed.
///
/// Required env: `DEPLOYER_PK` (= bridge owner), `CONTRACT_SALT`, `BRIDGE_OWNER`,
///   at least one of `LZ_ENDPOINT` / `HYPERLANE_MAILBOX`.
/// Optional: `CREATEX_ADDRESS` (reused if set), `ACTIVE_GATEWAY` ("lz" | "hyperlane"),
///   `REMOTE_CHAIN_IDS` (csv; step 4 is a no-op if unset), `REMOTE_EIDS` (csv, parallel) for LayerZero,
///   `WIRE_LOOPBACK` (route the local chain through the loopback adapter).
contract DeployAll is DeployCreateXDeterministic, DeployAdapters, DeployBridge, ConfigureBridge {
    function run() public override(DeployCreateXDeterministic, DeployAdapters, DeployBridge, ConfigureBridge) {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address owner = vm.envAddress("BRIDGE_OWNER");
        address deployer = vm.addr(deployerPk);

        address lzEndpoint = vm.envOr("LZ_ENDPOINT", address(0));
        address mailbox = vm.envOr("HYPERLANE_MAILBOX", address(0));
        require(lzEndpoint != address(0) || mailbox != address(0), "set LZ_ENDPOINT and/or HYPERLANE_MAILBOX");
        string memory active = vm.envOr("ACTIVE_GATEWAY", string(""));

        console2.log("Salt:", salt);

        vm.startBroadcast(deployerPk);

        // 1. CreateX factory — reuse CREATEX_ADDRESS if set, otherwise deploy a fresh one
        console2.log("[1/4] CreateX...");
        address createX = vm.envOr("CREATEX_ADDRESS", address(0));
        if (createX == address(0)) createX = deployCreateX(salt);
        console2.log("  CreateX:", createX);

        // 2. Deploy adapters (each only if its endpoint env is present)
        console2.log("[2/4] Deploy adapters...");
        address lz;
        address hl;
        address lb;
        if (lzEndpoint != address(0)) {
            lz = deployLayerZeroAdapter(createX, salt, deployer, owner, lzEndpoint);
            console2.log("  LayerZeroGatewayAdapter:", lz);
        }
        if (mailbox != address(0)) {
            hl = deployHyperlaneAdapter(createX, salt, deployer, owner, mailbox);
            console2.log("  HyperlaneGatewayAdapter:", hl);
        }
        if (vm.envOr("WIRE_LOOPBACK", false)) {
            lb = deployLoopbackAdapter(createX, salt, deployer, owner);
            console2.log("  LoopbackGatewayAdapter:", lb);
        }

        // 3. Deploy bridge with the active gateway
        console2.log("[3/4] Deploy bridge...");
        address gateway = _pickGateway(active, lz, hl);
        address bridge = deployBridge(createX, salt, deployer, owner, gateway);
        console2.log("  active gateway:", gateway);
        console2.log("  ERC7786Bridge:", bridge);

        // 4. Wire remotes (no-op when REMOTE_CHAIN_IDS is unset)
        console2.log("[4/4] Configure remotes...");
        configureBridge(bridge, lz, hl);
        if (lb != address(0)) configureLoopback(bridge, lb);

        vm.stopBroadcast();

        console2.log("=== DeployAll complete ===");
    }

    function _pickGateway(string memory active, address lz, address hl) internal pure returns (address) {
        bytes32 a = keccak256(bytes(active));
        if (a == keccak256("hyperlane")) {
            require(hl != address(0), "hyperlane adapter not deployed");
            return hl;
        }
        if (a == keccak256("lz")) {
            require(lz != address(0), "lz adapter not deployed");
            return lz;
        }
        return lz != address(0) ? lz : hl; // default: prefer LayerZero, else Hyperlane
    }
}
