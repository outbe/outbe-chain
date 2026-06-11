// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {LayerZeroRouter} from "../src/router/LayerZeroRouter.sol";
import {Auction} from "../src/Auction.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";
import {RouterAllocator} from "../src/allocators/RouterAllocator.sol";
import {TypeCasts} from "../src/libs/TypeCasts.sol";

/// @dev Post-deployment wiring script.
///
/// Connects all deployed contracts together:
///   - escrow.setAuthorizedCaller(router)
///   - auction.setRouter(router)
///   - allocator.addOperator(router)
///   - router peers
/// (router→auction is immutable, bound at router construction)
///
/// Required env vars:
///   DEPLOYER_PK            — deployer private key
///   ROUTER_ADDRESS         — deployed LayerZeroRouter address
///   AUCTION_ADDRESS        — deployed Auction address
///   ESCROW_ADDRESS         — deployed SolverEscrow address
///   ALLOCATOR_ADDRESS      — deployed RouterAllocator address
///   PEER_EIDS              — comma-separated LZ endpoint IDs
///   PEER_DOMAINS           — comma-separated domain IDs matching the EIDs
contract ConfigureAll is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address routerAddress = vm.envAddress("ROUTER_ADDRESS");
        address auctionAddress = vm.envAddress("AUCTION_ADDRESS");
        address escrowAddress = vm.envAddress("ESCROW_ADDRESS");
        address allocatorAddress = vm.envAddress("ALLOCATOR_ADDRESS");

        vm.startBroadcast(deployerPrivateKey);
        configureAll(routerAddress, auctionAddress, escrowAddress, allocatorAddress);
        vm.stopBroadcast();

        console2.log("=== Config complete ===");
    }

    function configureAll(
        address routerAddress,
        address auctionAddress,
        address escrowAddress,
        address allocatorAddress
    ) public {
        // 1. Escrow → Router
        SolverEscrow(escrowAddress).setAuthorizedCaller(routerAddress);
        console2.log("  escrow.setAuthorizedCaller done");

        // 2. Auction → Router
        Auction(auctionAddress).setRouter(routerAddress);
        console2.log("  auction.setRouter done");

        // 3. Allocator → Router
        RouterAllocator(allocatorAddress).addOperator(routerAddress);
        console2.log("  allocator.addOperator done");

        // Router → Auction binding is immutable (set at router construction); no setAuction step.
        LayerZeroRouter router = LayerZeroRouter(routerAddress);

        // 4. LayerZero peers
        uint256[] memory peerEids = vm.envUint("PEER_EIDS", ",");
        uint256[] memory peerDomains = vm.envUint("PEER_DOMAINS", ",");
        require(peerEids.length == peerDomains.length, "PEER_EIDS and PEER_DOMAINS length mismatch");

        bytes32 routerPeer = TypeCasts.addressToBytes32(routerAddress);
        for (uint256 i = 0; i < peerEids.length; i++) {
            router.setPeerWithDomain(uint32(peerEids[i]), routerPeer, uint32(peerDomains[i]));
            console2.log("  Peer set: EID", uint32(peerEids[i]), "-> domain", uint32(peerDomains[i]));
        }
    }
}
