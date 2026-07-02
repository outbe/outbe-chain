// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

contract MessengersUupsTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 1;
    uint32 internal constant BNB_CHAIN_ID = 2;

    address internal stranger = makeAddr("stranger");

    OriginMessenger internal origin;
    TargetMessenger internal target;

    function setUp() public {
        _setUpBridge();

        origin = DeployProxy.originMessenger(address(bridge), address(this), BNB_CHAIN_ID);
        target = DeployProxy.targetMessenger(address(bridge), address(this), OUTBE_CHAIN_ID);
    }

    function test_Initialize_SetsAdmin() public view {
        assertTrue(origin.hasRole(origin.DEFAULT_ADMIN_ROLE(), address(this)));
        assertTrue(target.hasRole(target.DEFAULT_ADMIN_ROLE(), address(this)));
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        origin.initialize(stranger);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        target.initialize(stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        OriginMessenger impl = new OriginMessenger(address(bridge), BNB_CHAIN_ID);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(address(this));

        TargetMessenger timpl = new TargetMessenger(address(bridge), OUTBE_CHAIN_ID);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        timpl.initialize(address(this));
    }

    function test_RevertWhen_InitializeZeroDelegate() public {
        OriginMessenger impl = new OriginMessenger(address(bridge), BNB_CHAIN_ID);
        bytes memory initData = abi.encodeCall(OriginMessenger.initialize, (address(0)));
        vm.expectRevert(abi.encodeWithSignature("ZeroAddress(string)", "delegate"));
        new ERC1967Proxy(address(impl), initData);
    }

    function test_RevertWhen_UpgradeByNonAdmin() public {
        TargetMessenger newImpl = new TargetMessenger(address(bridge), OUTBE_CHAIN_ID);
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        target.upgradeToAndCall(address(newImpl), "");

        OriginMessenger newOriginImpl = new OriginMessenger(address(bridge), BNB_CHAIN_ID);
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        origin.upgradeToAndCall(address(newOriginImpl), "");
    }

    function test_Upgrade_PreservesWiringPeersAndBridge() public {
        // Wire the origin side and register a remote messenger so post-upgrade state has something to prove.
        MockDesis desisMock = new MockDesis();
        address factory = makeAddr("factory");
        origin.wire(address(desisMock), factory);
        origin.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(target)));

        OriginMessenger newImpl = new OriginMessenger(address(bridge), BNB_CHAIN_ID);
        origin.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(origin), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(origin.desis(), address(desisMock));
        assertEq(origin.intexFactory(), factory);
        assertEq(origin.remoteMessenger(BNB_CHAIN_ID), _interop(BNB_CHAIN_ID, address(target)));
        assertEq(address(origin.BRIDGE()), address(bridge));
        assertTrue(origin.hasRole(origin.DEFAULT_ADMIN_ROLE(), address(this)));
        assertEq(origin.BNB_CHAIN_ID(), BNB_CHAIN_ID);
    }
}
