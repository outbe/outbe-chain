// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {IEscrowAdapter} from "@contracts/bnb/interfaces/IEscrowAdapter.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

contract EscrowAdapterUupsTest is Test {
    address internal admin = makeAddr("admin");
    address internal bridger = makeAddr("bridger");
    address internal stranger = makeAddr("stranger");

    EscrowAdapter internal escrow;

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
    }

    function test_Initialize_GrantsRoles() public view {
        assertTrue(escrow.hasRole(escrow.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(escrow.hasRole(escrow.RELAYER_ROLE(), bridger));
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        escrow.initialize(stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        EscrowAdapter impl = new EscrowAdapter();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(admin);
    }

    function test_RevertWhen_InitializeZeroAdmin() public {
        EscrowAdapter impl = new EscrowAdapter();
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "defaultAdmin"));
        new ERC1967Proxy(address(impl), abi.encodeCall(EscrowAdapter.initialize, (address(0))));
    }

    function test_RevertWhen_UpgradeByNonAdmin() public {
        EscrowAdapter newImpl = new EscrowAdapter();
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        escrow.upgradeToAndCall(address(newImpl), "");
    }

    function test_Upgrade_PreservesWiringAndCompactConfig() public {
        MockERC20 token = new MockERC20("Mock USD", "MUSD", 6);
        MockTheCompact compactMock = new MockTheCompact();
        MockVaultProvider vault = new MockVaultProvider();
        address auction = makeAddr("auction");

        vm.prank(admin);
        escrow.wire(auction, address(compactMock), address(vault), address(token));

        uint96 allocatorIdBefore = escrow.allocatorId();
        bytes12 lockTagBefore = escrow.lockTag();
        assertGt(uint256(allocatorIdBefore), 0);

        EscrowAdapter newImpl = new EscrowAdapter();
        vm.prank(admin);
        escrow.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(escrow), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(escrow.intexAuctionContract(), auction);
        assertEq(address(escrow.compact()), address(compactMock));
        assertEq(address(escrow.vaultProvider()), address(vault));
        assertEq(address(escrow.paymentToken()), address(token));
        assertEq(escrow.allocatorId(), allocatorIdBefore);
        assertEq(escrow.lockTag(), lockTagBefore);
        assertTrue(escrow.hasRole(escrow.AUCTION_ROLE(), auction));
    }
}
