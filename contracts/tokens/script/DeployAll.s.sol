// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";

import {DeployCreateXDeterministic} from "./0_DeployCreateX.s.sol";
import {DeployRoutes} from "./1_DeployRoutes.s.sol";
import {ConfigureRemotes} from "./2_ConfigureRemotes.s.sol";

/// @dev Full token-route deploy on one chain, in one command:
///   1. CreateX factory (reused when `CREATEX_ADDRESS` is set, or when one already sits at the deterministic address)
///   2. Every route in `script/routes/Routes.sol`
///   3. Remote wiring for each `REMOTE_CHAIN_IDS`
///
/// Every step self-checks against on-chain state, so a re-run on a finished chain sends no transactions. That is
/// deliberately per-step rather than one top-level short-circuit: a partial state (token deployed, Safe has not yet
/// executed `setTokenBridge`) must still be completed on the next run.
///
/// Addresses come from CREATE3 and depend only on (factory, salt, deployer) - so the same command on every chain
/// produces the same addresses, and step 3 is safe before the other chains exist.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `BRIDGE_ADDRESS`, `OUTBE_CHAIN_ID`, `EXTERNAL_CHAIN_ID`.
/// Optional env: `CREATEX_ADDRESS`, `OWNER_ADDRESS`, `ALLOW_EOA_OWNER`, `REMOTE_CHAIN_IDS`,
///   `INITIAL_MINT_AMOUNT`, `INITIAL_MINT_RECIPIENT`.
contract DeployAll is DeployCreateXDeterministic, DeployRoutes, ConfigureRemotes {
    function run() public override(DeployCreateXDeterministic, DeployRoutes, ConfigureRemotes) {
        string memory salt = vm.envString("CONTRACT_SALT");

        console2.log("Salt:", salt);
        console2.log(_isOutbe() ? "Role: Outbe" : "Role: external chain");

        vm.startBroadcast(_pk());

        console2.log("[1/3] CreateX...");
        address createX = vm.envOr("CREATEX_ADDRESS", address(0));
        if (createX == address(0)) createX = deployCreateX(salt);

        console2.log("[2/3] Routes...");
        deployRoutes(createX, salt);

        console2.log("[3/3] Configure remotes...");
        configureRemotes(createX, salt);

        vm.stopBroadcast();

        console2.log("=== DeployAll complete (identical on every chain) ===");
        console2.log("CREATEX_ADDRESS=", createX);
    }
}
