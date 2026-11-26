// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {CROSSCHAIN_BRIDGE} from "@precompiles/ICrosschainBridge.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {HandOver} from "../../deploy/HandOver.s.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";

/// @dev A test has no Outbe node, so the harness supplies the Interchain Account of CrosschainBridge.
contract HandOverHarness is HandOver {
    address public constant GOVERNANCE_ICA = address(uint160(0x1CA));
    uint32 public askedDomain;

    function _governanceAccount(uint32 domain) internal override returns (address) {
        askedDomain = domain;
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
        assertTrue(nft.hasRole(nft.ROUTER_ROLE(), address(0xB0B)), "operational roles stay");
    }

    function test_GovernanceOwner_IsCrosschainBridgeOnOutbeAndItsAccountElsewhere() public {
        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid));
        assertEq(script.governanceOwner(), CROSSCHAIN_BRIDGE, "Outbe");

        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid + 1));
        assertEq(script.governanceOwner(), address(uint160(0x1CA)), "other chain");
    }

    function test_OutbeIca_IsTheInterchainAccountOfOutbeOnThisChainEvenOnOutbe() public {
        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid));
        assertEq(script.governanceOwner(), CROSSCHAIN_BRIDGE, "governance on Outbe");
        assertEq(script.outbeIcaAccount(), script.GOVERNANCE_ICA(), "the messages come from the account");
        assertEq(script.askedDomain(), uint32(block.chainid), "the account on this chain");
    }

    function test_HandOver_LeavesTheIntexAuctionTargetRouterToItsOutbeIcaAndGovernance() public {
        IntexAuctionTargetRouter router = DeployProxy.targetRouter(address(0xB41D6E), address(script), 1);
        address ica = script.outbeIcaAccount();

        script.grantOutbeIca(address(router), ica);
        script.grantOutbeIca(address(router), ica);
        script.handOver(address(script), GOVERNANCE, address(router));

        assertTrue(router.hasRole(router.OUTBE_ICA_ROLE(), ica), "the ICA of Outbe takes the messages");
        assertTrue(router.hasRole(router.DEFAULT_ADMIN_ROLE(), GOVERNANCE), "governance is admin");
        assertFalse(router.hasRole(router.DEFAULT_ADMIN_ROLE(), address(script)), "deployer is not admin");
        assertFalse(router.hasRole(router.OUTBE_ICA_ROLE(), address(script)), "deployer takes no messages");
    }
}
