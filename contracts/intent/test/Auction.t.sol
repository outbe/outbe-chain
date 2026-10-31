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
        vm.warp(block.timestamp + auction.commitPeriod());
    }

    function _toEnd() internal {
        vm.warp(block.timestamp + auction.commitPeriod() + auction.revealPeriod());
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

        vm.warp(block.timestamp + auction.revealPeriod());
        (address winner, uint256 price) = auction.getWinner(orderId);
        assertEq(winner, address(4));
        assertEq(price, 200);
        assertEq(auction.getQuoteCount(orderId), 2);
    }

    /// A period change does not move the deadline of an auction that already started.
    function test_periodChangeDoesNotMoveInFlightDeadline() public {
        _commit(address(1), 100);
        uint256 deadline = auction.getRevealDeadline(orderId);

        auction.setCommitPeriod(1 hours);
        auction.setRevealPeriod(1 hours);

        assertEq(auction.getRevealDeadline(orderId), deadline);
        _toEnd();
        assertTrue(auction.isAuctionEnded(orderId));
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
