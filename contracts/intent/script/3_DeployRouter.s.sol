// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {Scope} from "the-compact/src/types/Scope.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {Router} from "../src/router/Router.sol";
import {RouterAllocator} from "../src/allocators/RouterAllocator.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";

/// @dev Deploys RouterAllocator + the composition {Router} (a UUPS proxy via Create3Factory). The Router talks to
///      the `crosschain` hub's `ERC7786Bridge`. This script has no LayerZero endpoint or eids. The
///      protocol lives on the bridge.
///
/// Required env vars:
///   DEPLOYER_PK      - deployer private key
///   CREATE3_FACTORY_ADDRESS - deployed Create3Factory (from contracts/shared)
///   CONTRACT_SALT    - salt string for deterministic deployment (default SALT_VERSION)
///   COMPACT_ADDRESS  - The Compact address
///   AUCTION_ADDRESS  - deployed Auction (immutable on the router)
///   BRIDGE_ADDRESS   - deployed ERC7786Bridge (the cross-chain hub facade)
///   ROUTER_OWNER     - contract owner (admin)
/// Optional: ESCROW_ADDRESS (address(0) disables collateral).
contract DeployRouter is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        address compact = vm.envAddress("COMPACT_ADDRESS");
        address auction = vm.envAddress("AUCTION_ADDRESS");
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        address escrow = vm.envOr("ESCROW_ADDRESS", address(0));

        vm.startBroadcast(deployerPrivateKey);
        (address router, address allocator) =
            deployRouter(factory, salt, bridge, compact, escrow, auction, vm.addr(deployerPrivateKey));
        vm.stopBroadcast();

        console2.log("RouterAllocator:", allocator);
        console2.log("Router:", router);
    }

    /// @dev The factory namespaces this with the caller, so this salt omits the deployer.
    function getRouterSalt(string memory salt) public pure returns (bytes32) {
        return keccak256(abi.encodePacked("Router", salt));
    }

    function deployRouter(
        address factory,
        string memory salt,
        address bridge,
        address compact,
        address escrow,
        address auction,
        address deployer
    ) public returns (address router, address allocatorAddr) {
        RouterAllocator allocator = RouterAllocator(
            address(
                new ERC1967Proxy(
                    address(new RouterAllocator(compact)), abi.encodeCall(RouterAllocator.initialize, (deployer))
                )
            )
        );
        bytes12 lockTag = allocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.ThirtyDays);
        allocatorAddr = address(allocator);

        address routerOwner = vm.envAddress("ROUTER_OWNER");
        bytes32 routerSalt = getRouterSalt(salt);
        address impl = address(new Router(bridge, compact, lockTag, escrow, auction));
        router = Create3Proxy.deploy(
            Create3Factory(factory), routerSalt, impl, abi.encodeCall(Router.initialize, (routerOwner))
        );
    }
}
