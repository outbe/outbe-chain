// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {CROSSCHAIN_GOV} from "@precompiles/ICrosschainGov.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {HandOver} from "../../deploy/HandOver.s.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";

/// @dev A test has no Outbe node, so the harness supplies the Interchain Account of CrosschainGov.
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

    function test_HandOver_GivesTheAdminRoleToGovernanceAndRemovesItFromTheDeployer() public {
        // The script is the deployer: it holds the admin role until the hand-over.
        IntexNFT1155 nft = DeployProxy.intexNFT1155(address(script), address(0xB0B));
        bytes32 admin = nft.DEFAULT_ADMIN_ROLE();

        script.handOver(address(script), GOVERNANCE, address(nft));
        script.handOver(address(script), GOVERNANCE, address(nft));

        assertTrue(nft.hasRole(admin, GOVERNANCE), "governance is admin");
        assertFalse(nft.hasRole(admin, address(script)), "deployer is not admin");
        assertTrue(nft.hasRole(nft.RELAYER_ROLE(), address(0xB0B)), "operational roles stay");
    }

    function test_GovernanceOwner_IsCrosschainGovOnOutbeAndItsAccountElsewhere() public {
        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid));
        assertEq(script.governanceOwner(), CROSSCHAIN_GOV, "Outbe");

        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid + 1));
        assertEq(script.governanceOwner(), address(uint160(0x1CA)), "other chain");
    }
}
