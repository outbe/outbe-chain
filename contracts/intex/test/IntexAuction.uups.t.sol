// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ReferenceCurrencyPriceLib} from "./utils/ReferenceCurrencyPriceLib.sol";
import {Test} from "forge-std/Test.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {DeployProxy} from "./utils/DeployProxy.sol";

contract IntexAuctionUupsTest is Test {
    address internal admin = makeAddr("admin");
    address internal bridger = makeAddr("bridger");
    address internal bidder = makeAddr("bidder");
    address internal stranger = makeAddr("stranger");

    IntexAuction internal auction;

    address internal escrow = makeAddr("escrow");
    address internal intex = makeAddr("intex");

    function setUp() public {
        auction = DeployProxy.intexAuction(admin, bridger, escrow, intex);
    }

    function _startAuction(uint32 emissionDay) internal {
        IIntexAuction.AuctionSchedule memory schedule = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + 1 hours),
            revealEnd: uint32(block.timestamp + 2 hours),
            issuanceEnd: uint32(block.timestamp + 3 hours)
        });
        IIntexAuction.AuctionParams memory params = IIntexAuction.AuctionParams({
            promisLoadMinor: 1000,
            minBidRate: 1,
            prices: ReferenceCurrencyPriceLib.onePriced(840, 1, 1),
            callTrigger: IIntexAuction.IntexCallTrigger({callWindow: 0, callThreshold: 0, callNoticePeriod: 0}),
            minBidQuantity: 1,
            bidBondMinor: 0
        });
        vm.prank(bridger);
        auction.startAuction(emissionDay, schedule, params);
    }

    function test_Initialize_SetsTheOwner() public view {
        assertEq(auction.owner(), admin);
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        auction.initialize(stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        IntexAuction impl = new IntexAuction(bridger, escrow, intex);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(admin);
    }

    function test_RevertWhen_InitializeZeroOwner() public {
        IntexAuction impl = new IntexAuction(bridger, escrow, intex);
        vm.expectRevert(abi.encodeWithSelector(IIntexAuction.ZeroAddress.selector, "owner"));
        new ERC1967Proxy(address(impl), abi.encodeCall(IntexAuction.initialize, (address(0))));
    }

    function test_RevertWhen_UpgradeByNonOwner() public {
        IntexAuction newImpl = new IntexAuction(bridger, escrow, intex);
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
        auction.upgradeToAndCall(address(newImpl), "");
    }

    function test_Upgrade_PreservesStateAndSwapsImplementation() public {
        _startAuction(20260612);
        vm.prank(bidder);
        auction.commitIntexAuctionBid(20260612, keccak256("commit"));

        IntexAuction newImpl = new IntexAuction(bridger, escrow, intex);
        vm.prank(admin);
        auction.upgradeToAndCall(address(newImpl), "");

        bytes32 implSlot = vm.load(address(auction), ERC1967Utils.IMPLEMENTATION_SLOT);
        assertEq(address(uint160(uint256(implSlot))), address(newImpl));
        assertEq(auction.committedIntexAuctionBidsByHash(20260612, bidder), keccak256("commit"));
        assertEq(address(auction.escrowAdapter()), escrow);
        assertEq(uint8(auction.auctionStage(20260612)), uint8(IIntexAuction.AuctionStage.CommittingBids));
        assertEq(auction.intexAuctionTargetRouter(), bridger);
        assertEq(auction.owner(), admin);
    }
}
