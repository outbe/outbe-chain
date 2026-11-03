// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {CROSSCHAIN_BRIDGE} from "@precompiles/ICrosschainBridge.sol";
import {HandOver} from "../script/5_HandOver.s.sol";
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
}
