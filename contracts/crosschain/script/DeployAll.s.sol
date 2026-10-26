// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";

import {DeployAdapters} from "./1_DeployAdapters.s.sol";
import {DeployBridge} from "./2_DeployBridge.s.sol";
import {ConfigureBridge} from "./3_ConfigureBridge.s.sol";
import {HandOver} from "./4_HandOver.s.sol";
import {Hub} from "./DeployBase.sol";

/// @dev Full hub deploy + wiring on one chain, in one shot:
///   1. Adapters - each deployed only when its endpoint env is set (`LZ_ENDPOINT` / `HYPERLANE_MAILBOX`)
///   2. Bridge (with the active gateway)
///   3. Wire remotes for each `REMOTE_CHAIN_IDS` (remote addresses == local CREATE3 addresses)
///   4. Hand-over - governance gets the owner of the bridge and of each adapter, and the LayerZero
///      delegate
/// The deployer owns the contracts during steps 1-3. This script does not deploy the CREATE3
/// factory. The factory is built and deployed once from contracts/shared.
/// Remote addresses are deterministic, so step 3 is safe even before other chains are deployed.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATE3_FACTORY_ADDRESS`, `OUTBE_CHAIN_ID`,
///   at least one of `LZ_ENDPOINT` / `HYPERLANE_MAILBOX`.
/// Optional: `ACTIVE_GATEWAY` ("lz" | "hyperlane"),
///   `REMOTE_CHAIN_IDS` (csv, step 3 is a no-op if unset), `REMOTE_EIDS` (csv, parallel) for
///   LayerZero,
///   `WIRE_LOOPBACK` (route the local chain through the loopback adapter),
///   `OUTBE_RPC` (an `rpc_endpoints` alias, default `outbe-testnet`).
contract DeployAll is DeployAdapters, DeployBridge, ConfigureBridge, HandOver {
    function run() public override(DeployAdapters, DeployBridge, ConfigureBridge, HandOver) {
        vm.startBroadcast(_pk());
        _deployAll();
        vm.stopBroadcast();

        console2.log("=== DeployAll complete ===");
    }

    /// @dev Steps 1-4. Opens no broadcast, so a test can call it directly.
    function _deployAll() internal returns (Hub memory hub) {
        string memory salt = vm.envString("CONTRACT_SALT");
        address deployer = _deployer();

        address lzEndpoint = vm.envOr("LZ_ENDPOINT", address(0));
        address mailbox = vm.envOr("HYPERLANE_MAILBOX", address(0));
        require(lzEndpoint != address(0) || mailbox != address(0), "set LZ_ENDPOINT and/or HYPERLANE_MAILBOX");
        string memory active = vm.envOr("ACTIVE_GATEWAY", string(""));

        // Resolved before the first deploy, so a missing OUTBE_CHAIN_ID or an unreachable
        // OUTBE_RPC stops the run before it deploys anything.
        address governance = _governanceOwner();

        console2.log("Salt:", salt);

        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        console2.log("Create3Factory:", factory);

        // 1. Deploy adapters (each only if its endpoint env is present)
        console2.log("[1/4] Deploy adapters...");
        if (lzEndpoint != address(0)) {
            hub.lz = deployLayerZeroAdapter(factory, salt, deployer, lzEndpoint);
            console2.log("  LayerZeroGatewayAdapter:", hub.lz);
        }
        if (mailbox != address(0)) {
            hub.hl = deployHyperlaneAdapter(factory, salt, deployer, mailbox);
            console2.log("  HyperlaneGatewayAdapter:", hub.hl);
        }
        if (vm.envOr("WIRE_LOOPBACK", false)) {
            hub.lb = deployLoopbackAdapter(factory, salt, deployer, deployer);
            console2.log("  LoopbackGatewayAdapter:", hub.lb);
        }

        // 2. Deploy bridge with the active gateway
        console2.log("[2/4] Deploy bridge...");
        address gateway = _pickGateway(active, hub.lz, hub.hl);
        hub.bridge = deployBridge(factory, salt, deployer, gateway);
        console2.log("  active gateway:", gateway);
        console2.log("  ERC7786Bridge:", hub.bridge);

        // 3. Wire remotes (no-op when REMOTE_CHAIN_IDS is unset)
        console2.log("[3/4] Configure remotes...");
        configureBridge(hub.bridge, hub.lz, hub.hl);
        if (hub.lb != address(0)) configureLoopback(hub.bridge, hub.lb);

        // 4. Give every owner role to governance and check the result
        console2.log("[4/4] Hand over to governance...");
        handOver(hub, governance);
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
        require(a == keccak256(""), "ACTIVE_GATEWAY must be lz or hyperlane");
        return lz != address(0) ? lz : hl; // default: prefer LayerZero, else Hyperlane
    }
}
