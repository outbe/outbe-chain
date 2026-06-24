// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {Scope} from "the-compact/src/types/Scope.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";

import {CreateX} from "./0_DeployCreateX.s.sol";
import {Router} from "../src/router/Router.sol";
import {RouterAllocator} from "../src/allocators/RouterAllocator.sol";

/// @dev Deploys RouterAllocator + the composition {Router} via CreateX. The Router talks to the `crosschain` hub's
///      `ERC7786Bridge` (no LayerZero endpoint / eids here — the protocol lives on the bridge).
///
/// Required env vars:
///   DEPLOYER_PK      — deployer private key
///   CREATEX_ADDRESS  — deployed CreateX factory
///   CONTRACT_SALT    — salt string for deterministic deployment
///   COMPACT_ADDRESS  — The Compact address
///   AUCTION_ADDRESS  — deployed Auction (immutable on the router)
///   BRIDGE_ADDRESS   — deployed ERC7786Bridge (the cross-chain hub facade)
///   ROUTER_OWNER     — contract owner (admin)
/// Optional: ESCROW_ADDRESS (address(0) disables collateral).
contract DeployRouter is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address createX = vm.envAddress("CREATEX_ADDRESS");
        string memory salt = vm.envString("CONTRACT_SALT");
        address compact = vm.envAddress("COMPACT_ADDRESS");
        address auction = vm.envAddress("AUCTION_ADDRESS");
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        address escrow = vm.envOr("ESCROW_ADDRESS", address(0));

        vm.startBroadcast(deployerPrivateKey);
        (address router, address allocator) = deployRouter(createX, salt, bridge, compact, escrow, auction);
        vm.stopBroadcast();

        console2.log("RouterAllocator:", allocator);
        console2.log("Router:", router);
    }

    function getRouterSaltHash(string memory salt) public view returns (bytes32) {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        return keccak256(abi.encodePacked("Router", salt, vm.addr(deployerPrivateKey)));
    }

    function deployRouter(
        address createX,
        string memory salt,
        address bridge,
        address compact,
        address escrow,
        address auction
    ) public returns (address router, address allocatorAddr) {
        RouterAllocator allocator = new RouterAllocator(compact);
        bytes12 lockTag = allocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.ThirtyDays);
        allocatorAddr = address(allocator);

        address routerOwner = vm.envAddress("ROUTER_OWNER");
        bytes32 saltHash = getRouterSaltHash(salt);
        bytes memory bytecode = abi.encodePacked(
            type(Router).creationCode, abi.encode(bridge, routerOwner, compact, lockTag, escrow, auction)
        );

        router = CreateX(createX).deployCreate3(saltHash, bytecode);
    }
}
