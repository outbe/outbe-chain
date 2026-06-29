// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console2} from "forge-std/console2.sol";

import {DeployCreateXDeterministic, CreateX} from "./0_DeployCreateX.s.sol";
import {DeploySolverEscrow} from "./1_DeploySolverEscrow.s.sol";
import {DeployAuction} from "./2_DeployAuction.s.sol";
import {DeployRouter} from "./3_DeployRouter.s.sol";
import {ConfigureAll} from "./4_ConfigureAll.s.sol";

/// @dev Full deployment + configuration in a single script.
///
/// Deploy order:
///   1. CreateX factory
///   2. SolverEscrow
///   3. Auction
///   4. RouterAllocator + composition Router (via CreateX) — talks to the ERC7786Bridge hub
///   5. Wire all contracts together (same-chain)
///
/// Required env vars:
///   DEPLOYER_PK      — deployer private key
///   CONTRACT_SALT    — salt string for deterministic deployment
///   BRIDGE_ADDRESS   — deployed ERC7786Bridge (the cross-chain hub facade)
///   ROUTER_OWNER     — contract owner (admin)
///   COMPACT_ADDRESS  — The Compact address
///   COLLATERAL_BPS   — collateral requirement in basis points (e.g. 1000 = 10%)
///
/// Cross-chain wiring (remote routers) is a separate step: ConfigureRouter.s.sol.
contract DeployAll is DeployCreateXDeterministic, DeployRouter, DeploySolverEscrow, DeployAuction, ConfigureAll {
    function run()
        public
        override(DeployCreateXDeterministic, DeployRouter, DeploySolverEscrow, DeployAuction, ConfigureAll)
    {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address compact = vm.envAddress("COMPACT_ADDRESS");
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        uint256 collateralBps = vm.envOr("COLLATERAL_BPS", uint256(1000));

        console2.log("Salt:", salt);

        vm.startBroadcast(deployerPrivateKey);

        // 1. CreateX factory — reuse CREATEX_ADDRESS if set, otherwise deploy a fresh one
        console2.log("[1/5] CreateX...");
        address createXAddr = vm.envOr("CREATEX_ADDRESS", address(0));
        if (createXAddr == address(0)) createXAddr = deployCreateX(salt);
        console2.log("  CreateX:", createXAddr);

        // Everything below is deterministic from (CreateX, salt, deployer). If the router already exists, the whole
        // stack is already deployed — skip it: re-deploying escrow/auction/allocator would waste gas and the router's
        // CREATE3 would revert on collision anyway.
        address routerAddr = CreateX(createXAddr).computeCreate3Address(getRouterSaltHash(salt));
        if (routerAddr.code.length != 0) {
            console2.log("Already deployed - skipping. Router:", routerAddr);
            vm.stopBroadcast();
            return;
        }

        // 2. Deploy SolverEscrow
        console2.log("[2/5] Deploy SolverEscrow...");
        address escrowAddress = deployEscrow(compact, collateralBps);
        console2.log("  SolverEscrow:", escrowAddress);

        // 3. Deploy Auction
        console2.log("[3/5] Deploy Auction...");
        address auctionAddress = deployAuction(vm.addr(deployerPrivateKey));
        console2.log("  Auction:", auctionAddress);

        // 4. Deploy RouterAllocator + Router via CreateX
        console2.log("[4/5] Deploy RouterAllocator + Router...");
        (address routerAddress, address allocatorAddress) =
            deployRouter(createXAddr, salt, bridge, compact, escrowAddress, auctionAddress);
        console2.log("  Router:", routerAddress);

        // 5. Wire all contracts together
        console2.log("[5/5] Configure all...");
        configureAll(routerAddress, auctionAddress, escrowAddress, allocatorAddress);

        vm.stopBroadcast();

        console2.log("=== DeployAll complete ===");
    }
}
