// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";

contract EscrowAdapterUupsTest is Test {
    address internal admin = makeAddr("admin");
    address internal bridger = makeAddr("bridger");
    address internal auction = makeAddr("auction");
    address internal stranger = makeAddr("stranger");

    EscrowAdapter internal escrow;
    MockTheCompact internal compactMock;
    MockWCOEN internal token;

    function setUp() public {
        compactMock = new MockTheCompact();
        token = new MockWCOEN();
        escrow = DeployProxy.escrowAdapter(admin, auction, bridger, address(compactMock), address(token));
    }

    function test_Initialize_SetsTheOwner() public view {
        assertEq(escrow.owner(), admin);
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        escrow.initialize(stranger, address(compactMock), address(token));
    }

    function test_RevertWhen_ImplementationInitialized() public {
        EscrowAdapter impl = new EscrowAdapter(auction, bridger);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(admin, address(compactMock), address(token));
    }

    function test_RevertWhen_InitializeZeroOwner() public {
        EscrowAdapter impl = new EscrowAdapter(auction, bridger);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroAddress.selector, "owner"));
        new ERC1967Proxy(
            address(impl), abi.encodeCall(EscrowAdapter.initialize, (address(0), address(compactMock), address(token)))
        );
    }

    function test_RevertWhen_UpgradeByNonOwner() public {
        EscrowAdapter newImpl = new EscrowAdapter(auction, bridger);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
        escrow.upgradeToAndCall(address(newImpl), "");
    }

    function test_Upgrade_PreservesLinksAndCompactConfig() public {
        uint96 allocatorIdBefore = escrow.allocatorId();
        bytes12 lockTagBefore = escrow.lockTag();
        assertGt(uint256(allocatorIdBefore), 0);

        EscrowAdapter newImpl = new EscrowAdapter(auction, bridger);
        vm.prank(admin);
        escrow.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(escrow), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(escrow.intexAuction(), auction);
        assertEq(escrow.intexAuctionTargetRouter(), bridger);
        assertEq(address(escrow.compact()), address(compactMock));
        assertEq(address(escrow.intexAuctionAsset()), address(token));
        assertEq(escrow.allocatorId(), allocatorIdBefore);
        assertEq(escrow.lockTag(), lockTagBefore);
        assertEq(escrow.owner(), admin);
    }
}
