// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

contract MessengersUupsTest is TestHelperOz5 {
    uint32 internal constant OUTBE_EID = 1;
    uint32 internal constant BNB_EID = 2;

    address internal stranger = makeAddr("stranger");

    OriginMessenger internal origin;
    TargetMessenger internal target;

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        origin = DeployProxy.originMessenger(address(endpoints[OUTBE_EID]), address(this), BNB_EID);
        target = DeployProxy.targetMessenger(address(endpoints[BNB_EID]), address(this), OUTBE_EID);
    }

    function test_Initialize_SetsOwnerAndAdmin() public view {
        assertEq(origin.owner(), address(this));
        assertEq(target.owner(), address(this));
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
        OriginMessenger impl = new OriginMessenger(address(endpoints[OUTBE_EID]), BNB_EID);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(address(this));

        TargetMessenger timpl = new TargetMessenger(address(endpoints[BNB_EID]), OUTBE_EID);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        timpl.initialize(address(this));
    }

    function test_RevertWhen_InitializeZeroDelegate() public {
        OriginMessenger impl = new OriginMessenger(address(endpoints[OUTBE_EID]), BNB_EID);
        bytes memory initData = abi.encodeCall(OriginMessenger.initialize, (address(0)));
        vm.expectRevert(abi.encodeWithSignature("ZeroAddress(string)", "delegate"));
        new ERC1967Proxy(address(impl), initData);
    }

    function test_RevertWhen_UpgradeByNonAdmin() public {
        TargetMessenger newImpl = new TargetMessenger(address(endpoints[BNB_EID]), OUTBE_EID);
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        target.upgradeToAndCall(address(newImpl), "");

        OriginMessenger newOriginImpl = new OriginMessenger(address(endpoints[OUTBE_EID]), BNB_EID);
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        origin.upgradeToAndCall(address(newOriginImpl), "");
    }

    function test_Upgrade_PreservesWiringPeersAndEndpoint() public {
        // Wire the origin side and set a peer so post-upgrade state has something to prove.
        MockDesis desisMock = new MockDesis();
        address factory = makeAddr("factory");
        origin.wire(address(desisMock), factory);
        origin.setPeer(BNB_EID, addressToBytes32(address(target)));

        OriginMessenger newImpl = new OriginMessenger(address(endpoints[OUTBE_EID]), BNB_EID);
        origin.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(origin), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(origin.desis(), address(desisMock));
        assertEq(origin.intexFactory(), factory);
        assertEq(origin.peers(BNB_EID), addressToBytes32(address(target)));
        assertEq(address(origin.endpoint()), address(endpoints[OUTBE_EID]));
        assertEq(origin.owner(), address(this));
        assertEq(origin.BNB_EID(), BNB_EID);
    }
}
