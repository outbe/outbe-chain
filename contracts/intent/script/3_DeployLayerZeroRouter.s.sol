// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {Scope} from "the-compact/src/types/Scope.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";

import {LayerZeroRouter} from "../src/router/LayerZeroRouter.sol";
import {RouterAllocator} from "../src/allocators/RouterAllocator.sol";
import {ICreateX} from "./utils/ICreateX.sol";

/// @dev Deploys RouterAllocator + LayerZeroRouter via CreateX.
///
/// 1. Deploys RouterAllocator and builds the lockTag.
/// 2. Deploys LayerZeroRouter via CreateX (compact, lockTag, escrow are immutables).
///
/// Standalone usage deploys with escrow=address(0).
/// Full deployment with all deps should use DeployAll.
///
/// Required env vars:
///   DEPLOYER_PK      — deployer private key
///   CREATEX_ADDRESS  — deployed CreateX factory
///   CONTRACT_SALT    — salt string for deterministic deployment
///   COMPACT_ADDRESS  — The Compact address
///   AUCTION_ADDRESS  — deployed Auction address (immutable on the router)
///   LZ_ENDPOINT      — LayerZero V2 endpoint address
///   ROUTER_OWNER     — contract owner (admin)
contract DeployLayerZeroRouter is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address createX = vm.envAddress("CREATEX_ADDRESS");
        string memory salt = vm.envString("CONTRACT_SALT");
        address compact = vm.envAddress("COMPACT_ADDRESS");
        address auction = vm.envAddress("AUCTION_ADDRESS");

        vm.startBroadcast(deployerPrivateKey);
        (address router, address allocator) = deployRouter(createX, salt, compact, address(0), auction);
        vm.stopBroadcast();

        console2.log("RouterAllocator deployed at:", allocator);
        console2.log("LayerZeroRouter deployed at:", router);
    }

    function getRouterSaltHash(string memory salt) public view returns (bytes32) {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        return keccak256(abi.encodePacked("LayerZeroRouter", salt, vm.addr(deployerPrivateKey)));
    }

    function deployRouter(address createX, string memory salt, address compact, address escrow, address auction)
        public
        returns (address router, address allocatorAddr)
    {
        // Deploy allocator and build lockTag
        RouterAllocator allocator = new RouterAllocator(compact);
        bytes12 lockTag = allocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.ThirtyDays);
        allocatorAddr = address(allocator);
        console2.log("  RouterAllocator:", allocatorAddr);

        // Deploy router via CreateX
        address lzEndpoint = vm.envAddress("LZ_ENDPOINT");
        address routerOwner = vm.envAddress("ROUTER_OWNER");

        bytes32 saltHash = getRouterSaltHash(salt);
        bytes memory bytecode = abi.encodePacked(
            type(LayerZeroRouter).creationCode, abi.encode(lzEndpoint, routerOwner, compact, lockTag, escrow, auction)
        );

        router = ICreateX(createX).deployCreate3(saltHash, bytecode);
    }
}
