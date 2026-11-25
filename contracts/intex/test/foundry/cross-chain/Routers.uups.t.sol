// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

contract RoutersUupsTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 1;

    address internal stranger = makeAddr("stranger");
    address internal erc20Router = makeAddr("erc20Router");

    TargetRouter internal target;

    function setUp() public {
        _setUpBridge();

        target = DeployProxy.targetRouter(address(bridge), address(this), OUTBE_CHAIN_ID, erc20Router);
    }

    function test_Initialize_SetsAdmin() public view {
        assertTrue(target.hasRole(target.DEFAULT_ADMIN_ROLE(), address(this)));
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        target.initialize(stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        TargetRouter timpl = new TargetRouter(address(bridge), OUTBE_CHAIN_ID, erc20Router);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        timpl.initialize(address(this));
    }

    function test_RevertWhen_InitializeZeroDelegate() public {
        TargetRouter impl = new TargetRouter(address(bridge), OUTBE_CHAIN_ID, erc20Router);
        bytes memory initData = abi.encodeCall(TargetRouter.initialize, (address(0)));
        vm.expectRevert(abi.encodeWithSignature("ZeroAddress(string)", "delegate"));
        new ERC1967Proxy(address(impl), initData);
    }

    function test_RevertWhen_UpgradeByNonAdmin() public {
        TargetRouter newImpl = new TargetRouter(address(bridge), OUTBE_CHAIN_ID, erc20Router);
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        target.upgradeToAndCall(address(newImpl), "");
    }

    function test_Upgrade_PreservesWiringRolesAndImmutables() public {
        address auction = makeAddr("auction");
        address intex = makeAddr("intex");
        address escrow = makeAddr("escrow");
        address ica = makeAddr("outbeIca");
        target.setDependencies(auction, intex, escrow);
        target.grantRole(target.OUTBE_ICA_ROLE(), ica);

        TargetRouter newImpl = new TargetRouter(address(bridge), OUTBE_CHAIN_ID, erc20Router);
        target.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(target), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(address(target.intexAuction()), auction);
        assertEq(address(target.intexContract()), intex);
        assertEq(address(target.escrow()), escrow);
        assertTrue(target.hasRole(target.OUTBE_ICA_ROLE(), ica));
        assertTrue(target.hasRole(target.DEFAULT_ADMIN_ROLE(), address(this)));
        assertEq(address(target.BRIDGE()), address(bridge));
        assertEq(address(target.erc20Router()), erc20Router);
        assertEq(target.OUTBE_CHAIN_ID(), OUTBE_CHAIN_ID);
    }
}
