// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";

contract ONFTAdaptersUupsTest is TestHelperOz5 {
    uint32 internal constant A_EID = 1;
    uint32 internal constant B_EID = 2;

    address internal admin = makeAddr("admin");
    address internal stranger = makeAddr("stranger");
    address internal tokenA = makeAddr("tokenA");

    ONFT1155Adapter internal adapter;
    ONFT1155AdapterBatch internal batch;

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        adapter = DeployProxy.onftAdapter(tokenA, address(endpoints[A_EID]), admin);
        batch = DeployProxy.onftAdapterBatch(tokenA, address(endpoints[A_EID]), admin);
    }

    function test_Initialize_SetsOwnerAndAdmin() public view {
        assertEq(adapter.owner(), admin);
        assertEq(batch.owner(), admin);
        assertTrue(batch.hasRole(batch.DEFAULT_ADMIN_ROLE(), admin));
        assertEq(address(adapter.token()), tokenA);
        assertEq(address(batch.token()), tokenA);
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        adapter.initialize(stranger);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        batch.initialize(stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        ONFT1155Adapter impl = new ONFT1155Adapter(tokenA, address(endpoints[A_EID]));
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(admin);

        ONFT1155AdapterBatch batchImpl = new ONFT1155AdapterBatch(tokenA, address(endpoints[A_EID]));
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        batchImpl.initialize(admin);
    }

    function test_RevertWhen_AdapterUpgradeByNonOwner() public {
        ONFT1155Adapter newImpl = new ONFT1155Adapter(tokenA, address(endpoints[A_EID]));
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
        adapter.upgradeToAndCall(address(newImpl), "");
    }

    function test_RevertWhen_BatchUpgradeByNonAdmin() public {
        ONFT1155AdapterBatch newImpl = new ONFT1155AdapterBatch(tokenA, address(endpoints[A_EID]));
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        batch.upgradeToAndCall(address(newImpl), "");
    }

    function test_Upgrade_PreservesPeersAndImmutables() public {
        vm.prank(admin);
        adapter.setPeer(B_EID, addressToBytes32(address(0xBEEF)));

        ONFT1155Adapter newImpl = new ONFT1155Adapter(tokenA, address(endpoints[A_EID]));
        vm.prank(admin);
        adapter.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(adapter), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(adapter.peers(B_EID), addressToBytes32(address(0xBEEF)));
        assertEq(address(adapter.token()), tokenA);
        assertEq(adapter.owner(), admin);
    }
}
