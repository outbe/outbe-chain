// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.28;

import {Test} from "forge-std/Test.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {MockAuctionEscrow} from "@test-mocks/MockAuctionEscrow.sol";

/// @dev Schedule snap on bridge signal: early reveal/clearing signal pulls the matching
///      stage boundary forward; late signal leaves the schedule untouched.
contract IntexAuctionScheduleSnapTest is Test {
    IntexAuction auction;
    MockAuctionEscrow escrow;

    address admin = address(1);
    address bridger = address(2);
    address bidder = address(3);

    uint32 constant COMMIT_OFFSET = 100;
    uint32 constant REVEAL_OFFSET = 200;
    uint32 constant ISSUANCE_OFFSET = 300;

    function setUp() public {
        auction = DeployProxy.intexAuction(admin, bridger);
        escrow = new MockAuctionEscrow();
        vm.startPrank(admin);
        auction.grantRole(auction.RELAYER_ROLE(), bridger);
        auction.wire(address(escrow));
        vm.stopPrank();
    }

    function _start(uint32 seriesId) internal {
        IIntexAuction.AuctionSchedule memory s = IIntexAuction.AuctionSchedule({
            commitEnd: uint32(block.timestamp + COMMIT_OFFSET),
            revealEnd: uint32(block.timestamp + REVEAL_OFFSET),
            issuanceEnd: uint32(block.timestamp + ISSUANCE_OFFSET)
        });
        IIntexAuction.AuctionParams memory p = IIntexAuction.AuctionParams({
            issuanceCurrency: 840,
            referenceCurrency: 840,
            promisLoadMinor: 1000,
            minIntexBidRate: 10,
            entryPriceMinor: 100,
            floorPriceMinor: 100,
            callPriceMinor: 100,
            callTrigger: IIntexAuction.IntexCallTrigger({windowDays: 0, thresholdDays: 0, intexCallPeriod: 0}),
            minIntexBidQuantity: 1
        });
        vm.prank(bridger);
        auction.auctionStart(seriesId, s, p);
    }

    function test_RevealSignal_Early_SnapsCommitEnd() public {
        uint32 seriesId = 20250130;
        _start(seriesId);
        IIntexAuction.AuctionData memory b = auction.getAuctionInfo(seriesId);
        uint32 signalTs = b.schedule.commitEnd - 10;
        vm.warp(signalTs);

        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);

        IIntexAuction.AuctionData memory a = auction.getAuctionInfo(seriesId);
        assertEq(a.schedule.commitEnd, signalTs);
        assertLt(a.schedule.commitEnd, b.schedule.commitEnd);
        assertEq(a.schedule.revealEnd, b.schedule.revealEnd);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.RevealingBids));
    }

    function test_RevealSignal_Late_LeavesCommitEnd() public {
        uint32 seriesId = 20250131;
        _start(seriesId);
        IIntexAuction.AuctionData memory b = auction.getAuctionInfo(seriesId);
        vm.warp(b.schedule.commitEnd + 5);

        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);

        IIntexAuction.AuctionData memory a = auction.getAuctionInfo(seriesId);
        assertEq(a.schedule.commitEnd, b.schedule.commitEnd);
        assertEq(a.schedule.revealEnd, b.schedule.revealEnd);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.RevealingBids));
    }

    function test_ClearingSignal_Early_SnapsRevealEnd() public {
        uint32 seriesId = 20250201;
        _start(seriesId);
        IIntexAuction.AuctionData memory b = auction.getAuctionInfo(seriesId);
        vm.warp(b.schedule.commitEnd + 1);
        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);

        uint32 signalTs = b.schedule.commitEnd + 20;
        vm.warp(signalTs);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        IIntexAuction.AuctionData memory a = auction.getAuctionInfo(seriesId);
        assertEq(a.schedule.revealEnd, signalTs);
        assertLt(a.schedule.revealEnd, b.schedule.revealEnd);
        assertEq(a.schedule.issuanceEnd, b.schedule.issuanceEnd);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Issuance));
    }

    function test_ClearingSignal_Late_LeavesRevealEnd() public {
        uint32 seriesId = 20250202;
        _start(seriesId);
        IIntexAuction.AuctionData memory b = auction.getAuctionInfo(seriesId);
        vm.warp(b.schedule.commitEnd + 1);
        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);

        vm.warp(b.schedule.revealEnd + 5);
        vm.prank(bridger);
        auction.startClearingStage(seriesId);

        IIntexAuction.AuctionData memory a = auction.getAuctionInfo(seriesId);
        assertEq(a.schedule.revealEnd, b.schedule.revealEnd);
        assertEq(a.schedule.issuanceEnd, b.schedule.issuanceEnd);
        assertEq(uint8(auction.getAuctionStage(seriesId)), uint8(IIntexAuction.AuctionStage.Issuance));
    }

    function test_RevealSignal_Early_BlocksLateCommitBid() public {
        uint32 seriesId = 20250203;
        _start(seriesId);

        vm.prank(bridger);
        auction.startRevealingBidsStage(seriesId, true);

        // commitEnd snapped to now → stage = RevealingBids → commitBid reverts on stage gate.
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexAuction.StageRequired.selector,
                IIntexAuction.AuctionStage.CommittingBids,
                IIntexAuction.AuctionStage.RevealingBids
            )
        );
        vm.prank(bidder);
        auction.commitBid(seriesId, keccak256("late"));
    }
}
