// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {CROSSCHAIN_BRIDGE} from "@precompiles/ICrosschainBridge.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {HandOver} from "../script/5_HandOver.s.sol";
import {IntentCreate3} from "../script/IntentCreate3.sol";
import {Auction} from "../src/Auction.sol";
import {Proxies} from "./utils/Proxies.sol";

/// @dev A test has no Outbe node, so the harness supplies the Interchain Account of CrosschainBridge.
contract HandOverHarness is HandOver {
    address internal constant GOVERNANCE_ICA = address(uint160(0x1CA));

    function _governanceAccount(uint32) internal pure override returns (address) {
        return GOVERNANCE_ICA;
    }
}

contract HandOverTest is Test {
    address internal constant GOVERNANCE = address(uint160(0x60F));

    HandOverHarness internal script;

    function setUp() public {
        script = new HandOverHarness();
    }

    function test_HandOver_GivesEveryContractToGovernance() public {
        // The script is the deployer: it owns every contract until the hand-over. Every intent
        // contract is a plain Ownable, so five Auction proxies stand for the five contracts.
        address owner = address(script);
        address[5] memory owned = [
            address(Proxies.auction(owner)),
            address(Proxies.auction(owner)),
            address(Proxies.auction(owner)),
            address(Proxies.auction(owner)),
            address(Proxies.auction(owner))
        ];

        script.handOver(GOVERNANCE, owned);
        script.handOver(GOVERNANCE, owned);

        for (uint256 i = 0; i < owned.length; i++) {
            assertEq(Ownable(owned[i]).owner(), GOVERNANCE);
        }
    }

    function test_GovernanceOwner_IsCrosschainBridgeOnOutbeAndItsAccountElsewhere() public {
        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid));
        assertEq(script.governanceOwner(), CROSSCHAIN_BRIDGE, "Outbe");

        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid + 1));
        assertEq(script.governanceOwner(), address(uint160(0x1CA)), "other chain");
    }

    /// @dev A re-run of a handed-over stack: a Router governance owns ends the run before any send.
    function test_Run_StopsWhenTheRouterIsHandedOver() public {
        Create3Factory factory = new Create3Factory();
        vm.prank(vm.addr(1));
        Create3Proxy.deploy(
            factory,
            IntentCreate3.salt(IntentCreate3.ROUTER, "v-test"),
            address(new Auction()),
            abi.encodeCall(Auction.initialize, (CROSSCHAIN_BRIDGE))
        );
        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid));
        vm.setEnv("DEPLOYER_PK", "1");
        vm.setEnv("CREATE3_FACTORY_ADDRESS", vm.toString(address(factory)));
        vm.setEnv("CONTRACT_SALT", "v-test");

        script.run();
    }

    function test_Stack_PredictsTheRouterLast() public {
        Create3Factory factory = new Create3Factory();
        vm.setEnv("CREATE3_FACTORY_ADDRESS", vm.toString(address(factory)));
        vm.setEnv("CONTRACT_SALT", "v-test");

        address[5] memory stack = script.stack(address(this));

        assertEq(stack[0], IntentCreate3.predict(address(factory), address(this), IntentCreate3.AUCTION, "v-test"));
        assertEq(stack[4], IntentCreate3.predict(address(factory), address(this), IntentCreate3.ROUTER, "v-test"));
    }
}
