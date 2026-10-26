// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";

import {DeployRoutes} from "./1_DeployRoutes.s.sol";

/// @dev Full token-route deploy on one chain, in one command: every route in `script/routes/Routes.sol`.
/// The CREATE3 factory is not deployed here: it is built and deployed once from contracts/shared,
/// and passed in as `CREATE3_FACTORY_ADDRESS`.
///
/// Every step self-checks against on-chain state, so a re-run on a finished chain sends no transactions. That is
/// deliberately per-step rather than one top-level short-circuit: a partial state (token deployed, bridge not yet)
/// must still be completed on the next run.
///
/// Addresses come from CREATE3 and depend only on (factory, salt, deployer). Thus the same command
/// on every chain produces the same addresses. Each bridge gets its peers in the constructor, so no
/// wiring step follows.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `BRIDGE_ADDRESS`, `OUTBE_CHAIN_ID`, `EXTERNAL_CHAIN_ID`,
///   `REMOTE_CHAIN_IDS`. A chain other than Outbe also reads the bridge owner from Outbe (`OUTBE_RPC`).
/// Optional env: `INITIAL_MINT_AMOUNT`, `INITIAL_MINT_RECIPIENT`.
contract DeployAll is DeployRoutes {
    function run() public override {
        string memory salt = vm.envString("CONTRACT_SALT");

        console2.log("Salt:", salt);
        console2.log(_isOutbe() ? "Role: Outbe" : "Role: external chain");

        vm.startBroadcast(_pk());

        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        console2.log("Create3Factory:", factory);

        console2.log("Routes...");
        deployRoutes(factory, salt);

        vm.stopBroadcast();

        console2.log("=== DeployAll complete (identical on every chain) ===");
        console2.log("CREATE3_FACTORY_ADDRESS=", factory);
    }
}
