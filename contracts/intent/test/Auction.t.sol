// SPDX-License-Identifier: MIT
pragma solidity ^0.8.25;

import {Test} from "forge-std/Test.sol";

import {Auction} from "../src/Auction.sol";
import {IAuction} from "../src/interfaces/IAuction.sol";
import {OrderData, OrderEncoder} from "../src/libs/OrderEncoder.sol";
import {Proxies} from "./utils/Proxies.sol";

contract AuctionTest is Test {
    Auction internal auction;
    bytes32 internal orderId;
    bytes internal originData;

    bytes32 constant SALT = bytes32(uint256(42));

    function setUp() public {
        auction = Proxies.auction(address(this));
        _setOrder(0);
    }

    function _setOrder(uint256 amountOut) internal {
        OrderData memory data;
        data.amountOut = amountOut;
        data.fillDeadline = uint32(block.timestamp + 1 days);
        orderId = OrderEncoder.id(data);
        originData = OrderEncoder.encode(data);
    }

    function _commit(address solver, uint256 outputAmount) internal {
        vm.prank(solver);
        auction.commit(orderId, keccak256(abi.encode(orderId, solver, outputAmount, SALT)));
    }

    function _reveal(address solver, uint256 outputAmount) internal {
        vm.prank(solver);
        auction.reveal(orderId, outputAmount, SALT, originData);
    }

    function _toReveal() internal {
        vm.roll(vm.getBlockNumber() + auction.commitPeriod());
    }

    function _toEnd() internal {
        vm.roll(vm.getBlockNumber() + auction.commitPeriod() + auction.revealPeriod());
    }

    /// Low bids revealed first cannot exclude a later high bid: it replaces the lowest quote.
    function test_fullQuoteListEvictsLowestBid() public {
        auction.setMaxQuotesPerOrder(2);
        _commit(address(1), 100);
        _commit(address(2), 200);
        _commit(address(3), 50);
        _commit(address(4), 400);

        _toReveal();
        _reveal(address(1), 100);
        _reveal(address(2), 200);
        vm.expectRevert(IAuction.MaxQuotesReached.selector);
        _reveal(address(3), 50);
        _reveal(address(4), 400);

        vm.roll(vm.getBlockNumber() + auction.revealPeriod());
        (address winner, uint256 price) = auction.getWinner(orderId);
        assertEq(winner, address(4));
        assertEq(price, 200);
        assertEq(auction.getQuoteCount(orderId), 2);
    }

    /// A period change does not move the deadline of an auction that already started.
    function test_periodChangeDoesNotMoveInFlightDeadline() public {
        _commit(address(1), 100);

        uint256 commitDeadline = auction.getCommitDeadline(orderId);
        uint256 deadline = auction.getRevealDeadline(orderId);

        auction.setCommitPeriod(100);
        auction.setRevealPeriod(100);

        assertEq(auction.getCommitDeadline(orderId), commitDeadline);
        assertEq(auction.getRevealDeadline(orderId), deadline);
        _toEnd();
        assertTrue(auction.isAuctionEnded(orderId));
    }

    function test_defaultPeriodsAreFiveBlocks() public view {
        assertEq(auction.commitPeriod(), 5);
        assertEq(auction.revealPeriod(), 5);
        assertEq(auction.maxQuotesPerOrder(), 10);
    }

    /// The window starts at the block of the first commit. The deadlines are block numbers.
    function test_windowIsMeasuredInBlocks() public {
        assertEq(auction.auctionStartedAt(orderId), 0);
        assertEq(auction.getCommitDeadline(orderId), 0);
        assertEq(auction.getRevealDeadline(orderId), 0);

        vm.roll(1000);
        vm.warp(1_700_000_000);
        _commit(address(1), 100);

        assertEq(auction.auctionStartedAt(orderId), 1000);
        assertEq(auction.getCommitDeadline(orderId), 1000 + auction.commitPeriod());
        assertEq(auction.getRevealDeadline(orderId), 1000 + auction.commitPeriod() + auction.revealPeriod());
    }

    /// Time that passes without a new block does not move the auction out of the commit phase.
    function test_timeWithoutNewBlockKeepsCommitPhase() public {
        _commit(address(1), 100);

        vm.warp(block.timestamp + 1 days);

        _commit(address(2), 200);
        assertFalse(auction.isAuctionEnded(orderId));
        vm.prank(address(1));
        vm.expectRevert(IAuction.RevealPhaseNotActive.selector);
        auction.reveal(orderId, 100, SALT, originData);
    }

    /// The reveal phase starts at block startedAt + commitPeriod, not one block before.
    function test_revealPhaseStartsAtCommitDeadlineBlock() public {
        _commit(address(1), 100);
        uint256 commitDeadline = auction.auctionStartedAt(orderId) + auction.commitPeriod();
        assertEq(auction.getCommitDeadline(orderId), commitDeadline);

        vm.roll(commitDeadline - 1);
        vm.prank(address(1));
        vm.expectRevert(IAuction.RevealPhaseNotActive.selector);
        auction.reveal(orderId, 100, SALT, originData);

        vm.roll(commitDeadline);
        vm.prank(address(2));
        vm.expectRevert(IAuction.CommitPhaseEnded.selector);
        auction.commit(orderId, bytes32(uint256(1)));
        _reveal(address(1), 100);
        assertEq(auction.getQuoteCount(orderId), 1);
    }

    /// The auction ends at block startedAt + commitPeriod + revealPeriod, not one block before.
    function test_auctionEndsAtRevealDeadlineBlock() public {
        _commit(address(1), 100);
        uint256 revealDeadline = auction.auctionStartedAt(orderId) + auction.commitPeriod() + auction.revealPeriod();
        assertEq(auction.getRevealDeadline(orderId), revealDeadline);
        _toReveal();
        _reveal(address(1), 100);

        vm.roll(revealDeadline - 1);
        assertFalse(auction.isAuctionEnded(orderId));
        vm.expectRevert(IAuction.RevealNotEnded.selector);
        auction.getWinner(orderId);

        vm.roll(revealDeadline);
        assertTrue(auction.isAuctionEnded(orderId));
        (address winner, uint256 price) = auction.getWinner(orderId);
        assertEq(winner, address(1));
        assertEq(price, 100);
    }

    function test_setPeriodRejectsZeroAndAboveUint64() public {
        vm.expectRevert(IAuction.InvalidPeriod.selector);
        auction.setCommitPeriod(0);
        vm.expectRevert(IAuction.InvalidPeriod.selector);
        auction.setRevealPeriod(0);
        vm.expectRevert(IAuction.InvalidPeriod.selector);
        auction.setCommitPeriod(uint256(type(uint64).max) + 1);
        vm.expectRevert(IAuction.InvalidPeriod.selector);
        auction.setRevealPeriod(uint256(type(uint64).max) + 1);
    }

    function test_setMaxQuotesRejectsValueAboveCeiling() public {
        uint256 maxQuotes = auction.MAX_QUOTES_PER_ORDER();
        vm.expectRevert(IAuction.InvalidMaxQuotes.selector);
        auction.setMaxQuotesPerOrder(maxQuotes + 1);
    }

    /// A zero bid on an order with a zero floor would make getWinner return address(0).
    function test_revealRejectsZeroOutput() public {
        _commit(address(1), 0);
        _toReveal();
        vm.expectRevert(IAuction.ZeroOutputAmount.selector);
        _reveal(address(1), 0);
    }

    function test_renounceOwnershipReverts() public {
        vm.expectRevert(IAuction.RenounceDisabled.selector);
        auction.renounceOwnership();
        assertEq(auction.owner(), address(this));
    }
}
