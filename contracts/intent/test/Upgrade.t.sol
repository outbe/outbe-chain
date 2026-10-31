// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";
import {Scope} from "the-compact/src/types/Scope.sol";

import {Auction} from "../src/Auction.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";
import {RouterAllocator} from "../src/allocators/RouterAllocator.sol";
import {SolverAllocator} from "../src/allocators/SolverAllocator.sol";
import {Router} from "../src/router/Router.sol";
import {MockERC7786Bridge} from "./mocks/MockERC7786Bridge.sol";
import {MockTheCompact} from "./mocks/MockTheCompact.sol";
import {Proxies} from "./utils/Proxies.sol";

/// @dev Each contract keeps its owner and state across an upgrade, and only the owner can upgrade.
contract UpgradeTest is Test {
    uint32 internal constant REMOTE_DOMAIN = 97;

    address internal owner = address(this);
    address internal stranger = makeAddr("stranger");
    address internal operator = makeAddr("operator");

    MockTheCompact internal compact;

    function setUp() public {
        compact = new MockTheCompact();
    }

    function test_Auction_KeepsStateAcrossUpgrade() public {
        Auction auction = Proxies.auction(owner);
        assertEq(auction.commitPeriod(), 10 seconds, "default commit period");
        auction.setRouter(operator);
        auction.setCommitPeriod(30 seconds);

        _upgrade(address(auction), address(new Auction()));

        assertEq(auction.owner(), owner, "owner");
        assertEq(auction.router(), operator, "router");
        assertEq(auction.commitPeriod(), 30 seconds, "commit period");
        assertEq(auction.revealPeriod(), 10 seconds, "reveal period");
        assertEq(auction.maxQuotesPerOrder(), 10, "max quotes");
    }

    function test_SolverEscrow_KeepsStateAcrossUpgrade() public {
        SolverAllocator allocator = Proxies.solverAllocator(address(compact), owner);
        bytes12 lockTag = allocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.TenMinutes);
        SolverEscrow escrow = Proxies.solverEscrow(address(compact), lockTag, 1000, owner);
        escrow.setAuthorizedCaller(operator);

        _upgrade(address(escrow), address(new SolverEscrow(address(compact), lockTag)));

        assertEq(escrow.owner(), owner, "owner");
        assertEq(escrow.AUTHORIZED_CALLER(), operator, "authorized caller");
        assertEq(escrow.collateralBps(), 1000, "collateral bps");
        assertEq(escrow.LOCK_TAG(), lockTag, "lock tag");
    }

    function test_SolverAllocator_KeepsStateAcrossUpgrade() public {
        SolverAllocator allocator = Proxies.solverAllocator(address(compact), owner);
        uint96 allocatorId = allocator.ALLOCATOR_ID();
        allocator.setArbiter(operator);

        _upgrade(address(allocator), address(new SolverAllocator(address(compact))));

        assertGt(uint256(allocatorId), 0, "registered");
        assertEq(allocator.ALLOCATOR_ID(), allocatorId, "allocator id");
        assertEq(allocator.arbiter(), operator, "arbiter");
        assertEq(allocator.owner(), owner, "owner");
    }

    function test_RouterAllocator_KeepsStateAcrossUpgrade() public {
        RouterAllocator allocator = _routerAllocator();
        uint96 allocatorId = allocator.ALLOCATOR_ID();
        allocator.addOperator(operator);

        _upgrade(address(allocator), address(new RouterAllocator(address(compact))));

        assertGt(uint256(allocatorId), 0, "registered");
        assertEq(allocator.ALLOCATOR_ID(), allocatorId, "allocator id");
        assertTrue(allocator.authorizedOperators(operator), "operator");
        assertEq(allocator.owner(), owner, "owner");
    }

    function test_Router_KeepsStateAcrossUpgrade() public {
        (Router router,) = _router();
        bytes memory remote = abi.encodePacked("remote router");
        router.setRemoteRouter(REMOTE_DOMAIN, remote);
        address implementation = address(
            new Router(
                address(router.bridge()), address(compact), router.LOCK_TAG(), address(0), address(router.AUCTION())
            )
        );

        _upgrade(address(router), implementation);

        assertEq(router.owner(), owner, "owner");
        assertEq(router.remoteRouters(REMOTE_DOMAIN), remote, "remote router");
    }

    function test_RevertWhen_NotOwnerUpgrades() public {
        address[5] memory proxies = _deployAll();
        address implementation = address(new Auction());
        for (uint256 i = 0; i < proxies.length; i++) {
            vm.prank(stranger);
            vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
            UUPSUpgradeable(proxies[i]).upgradeToAndCall(implementation, "");
        }
    }

    function test_RevertWhen_InitializedAgain() public {
        address[5] memory proxies = _deployAll();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        SolverEscrow(proxies[1]).initialize(stranger, 1000);
        for (uint256 i = 0; i < proxies.length; i++) {
            if (i == 1) continue;
            vm.expectRevert(Initializable.InvalidInitialization.selector);
            Auction(proxies[i]).initialize(stranger);
        }
    }

    function test_RevertWhen_ImplementationInitialized() public {
        Auction implementation = new Auction();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        implementation.initialize(stranger);
    }

    function _deployAll() internal returns (address[5] memory) {
        SolverAllocator solverAllocator = Proxies.solverAllocator(address(compact), owner);
        bytes12 lockTag = solverAllocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.TenMinutes);
        (Router router,) = _router();
        return [
            address(Proxies.auction(owner)),
            address(Proxies.solverEscrow(address(compact), lockTag, 1000, owner)),
            address(solverAllocator),
            address(_routerAllocator()),
            address(router)
        ];
    }

    function _routerAllocator() internal returns (RouterAllocator) {
        return RouterAllocator(
            Proxies.proxy(
                address(new RouterAllocator(address(compact))), abi.encodeCall(RouterAllocator.initialize, (owner))
            )
        );
    }

    function _router() internal returns (Router router, address implementation) {
        address bridge = address(new MockERC7786Bridge(block.chainid));
        address auction = address(Proxies.auction(owner));
        implementation = address(new Router(bridge, address(compact), bytes12(uint96(1)), address(0), auction));
        router = Router(Proxies.proxy(implementation, abi.encodeCall(Router.initialize, (owner))));
    }

    function _upgrade(address proxy, address implementation) internal {
        UUPSUpgradeable(proxy).upgradeToAndCall(implementation, "");
        assertEq(address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT)))), implementation, "impl");
    }
}
