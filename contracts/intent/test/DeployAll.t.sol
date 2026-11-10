// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {DeployAll} from "../script/DeployAll.s.sol";
import {IntentCreate3} from "../script/IntentCreate3.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";
import {SolverAllocator} from "../src/allocators/SolverAllocator.sol";
import {Router} from "../src/router/Router.sol";
import {MockERC7786Bridge} from "./mocks/MockERC7786Bridge.sol";
import {MockTheCompact} from "./mocks/MockTheCompact.sol";

/// @dev The deploy steps place every intent proxy at its CREATE3 address, and a second run keeps it.
contract DeployAllTest is Test {
    string internal constant SALT = "v-test";

    DeployAll internal script;
    address internal factory;
    address internal compact;
    address internal bridge;

    function setUp() public {
        script = new DeployAll();
        factory = address(new Create3Factory());
        compact = address(new MockTheCompact());
        bridge = address(new MockERC7786Bridge(block.chainid));
        vm.setEnv("ROUTER_OWNER", vm.toString(address(script)));
    }

    function test_Deploy_PutsEveryProxyAtItsCreate3Address() public {
        (address escrow, address auction, address router, address allocator) = _deploy();

        assertEq(escrow, _predict(IntentCreate3.SOLVER_ESCROW), "escrow");
        assertEq(auction, _predict(IntentCreate3.AUCTION), "auction");
        assertEq(router, _predict(IntentCreate3.ROUTER), "router");
        assertEq(allocator, _predict(IntentCreate3.ROUTER_ALLOCATOR), "router allocator");
        assertEq(SolverAllocator(_predict(IntentCreate3.SOLVER_ALLOCATOR)).arbiter(), escrow, "arbiter");
        assertEq(address(SolverEscrow(escrow).COMPACT()), compact, "escrow compact");
        assertEq(address(Router(router).AUCTION()), auction, "router auction");
        assertEq(address(Router(router).SOLVER_ESCROW()), escrow, "router escrow");
    }

    function test_Deploy_SecondRunKeepsTheStack() public {
        (address escrow, address auction, address router, address allocator) = _deploy();

        (address escrow2, address auction2, address router2, address allocator2) = _deploy();

        assertEq(escrow2, escrow, "escrow");
        assertEq(auction2, auction, "auction");
        assertEq(router2, router, "router");
        assertEq(allocator2, allocator, "router allocator");
    }

    function _deploy() internal returns (address escrow, address auction, address router, address allocator) {
        address deployer = address(script);
        escrow = script.deployEscrow(factory, SALT, compact, 1000, deployer);
        auction = script.deployAuction(factory, SALT, deployer, deployer);
        (router, allocator) = script.deployRouter(factory, SALT, bridge, compact, escrow, auction, deployer);
    }

    function _predict(string memory label) internal view returns (address) {
        return IntentCreate3.predict(factory, address(script), label, SALT);
    }
}
