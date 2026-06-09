// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import { console2 } from "forge-std/console2.sol";

import { DeployCreateXDeterministic } from "./0_DeployCreateX.s.sol";
import { DeploySolverEscrow } from "./1_DeploySolverEscrow.s.sol";
import { DeployAuction } from "./2_DeployAuction.s.sol";
import { DeployLayerZeroRouter } from "./3_DeployLayerZeroRouter.s.sol";
import { ConfigureAll } from "./4_ConfigureAll.s.sol";

/// @dev Full deployment + configuration in a single script.
///
/// Deploy order:
///   1. CreateX factory
///   2. SolverEscrow
///   3. Auction
///   4. RouterAllocator + LayerZeroRouter via CreateX
///   5. Wire all contracts together
///
/// Required env vars:
///   DEPLOYER_PK      — deployer private key
///   CONTRACT_SALT    — salt string for deterministic deployment
///   LZ_ENDPOINT      — LayerZero V2 endpoint address
///   ROUTER_OWNER     — contract owner (admin)
///   COMPACT_ADDRESS  — The Compact address
///   COLLATERAL_BPS   — collateral requirement in basis points (e.g. 1000 = 10%)
///   PEER_EIDS        — comma-separated LZ endpoint IDs
///   PEER_DOMAINS     — comma-separated domain IDs
contract DeployAll is
    DeployCreateXDeterministic,
    DeployLayerZeroRouter,
    DeploySolverEscrow,
    DeployAuction,
    ConfigureAll
{
    function run()
        public
        override(DeployCreateXDeterministic, DeployLayerZeroRouter, DeploySolverEscrow, DeployAuction, ConfigureAll)
    {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address compact = vm.envAddress("COMPACT_ADDRESS");
        uint256 collateralBps = vm.envOr("COLLATERAL_BPS", uint256(1000));

        console2.log("Salt:", salt);

        vm.startBroadcast(deployerPrivateKey);

        // 1. Deploy CreateX factory
        console2.log("[1/5] Deploy CreateX...");
        address createXAddr = deployCreateX(salt);
        console2.log("  CreateX:", createXAddr);

        // 2. Deploy SolverEscrow
        console2.log("[2/5] Deploy SolverEscrow...");
        address escrowAddress = deployEscrow(compact, collateralBps);
        console2.log("  SolverEscrow:", escrowAddress);

        // 3. Deploy Auction
        console2.log("[3/5] Deploy Auction...");
        address auctionAddress = deployAuction(vm.addr(deployerPrivateKey));
        console2.log("  Auction:", auctionAddress);

        // 4. Deploy RouterAllocator + LayerZeroRouter via CreateX
        console2.log("[4/5] Deploy RouterAllocator + LayerZeroRouter...");
        (address routerAddress, address allocatorAddress) =
            deployRouter(createXAddr, salt, compact, escrowAddress, auctionAddress);
        console2.log("  LayerZeroRouter:", routerAddress);

        // 5. Wire all contracts together
        console2.log("[5/5] Configure all...");
        configureAll(routerAddress, auctionAddress, escrowAddress, allocatorAddress);

        vm.stopBroadcast();

        console2.log("=== DeployAll complete ===");
    }
}
