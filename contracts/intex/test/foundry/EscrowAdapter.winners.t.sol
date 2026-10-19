// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev A refund chunk names a day's winners and its clearing terms. The escrow computes each
///      payment itself.
contract EscrowAdapterWinnersTest is Test {
    /// @dev The escrow's lock for `q` Intexes at rate `r` of the protocol basis `b`.
    function _protocol(uint256 q, uint256 b, uint256 r) internal pure returns (uint256) {
        return q * b * r / 1e6;
    }

    // MockWCOEN has 18 decimals: 1e12 token units per protocol unit.
    uint256 internal constant WCOEN_SCALE = 1e12;
    EscrowAdapter internal escrow;
    MockTheCompact internal compact;
    MockWCOEN internal token;

    address internal admin = address(1);
    address internal relayer = address(2);
    address internal auction = address(3);
    address internal proceeds = address(4);
    address internal alice = address(5);
    address internal bob = address(6);

    uint32 internal constant DAY = 1;
    uint128 internal constant BASIS = 1_000_000;
    uint64 internal constant CLEARING_RATE = 600_000;
    bytes32 internal constant RECEIVE_ID = bytes32(uint256(0xC0FFEE));

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, relayer);
        compact = new MockTheCompact();
        compact.setResetPeriodSeconds(0);
        token = new MockWCOEN();

        vm.startPrank(admin);
        escrow.wire(auction, address(compact), address(token));
        escrow.setProceedsRecipient(proceeds);
        vm.stopPrank();
    }

    function _refund(uint32 worldwideDay, address bidder) internal {
        vm.prank(auction);
        escrow.releaseRefund(worldwideDay, bidder);
    }

    function _lock(address who, uint32 bidRate, uint16 quantity) internal returns (uint128 amount) {
        amount = uint128(_protocol(quantity, BASIS, bidRate) * WCOEN_SCALE);
        token.mint(who, amount);
        vm.prank(who);
        token.approve(address(escrow), type(uint256).max);
        vm.prank(auction);
        escrow.lockFunds(DAY, who, BASIS, bidRate, quantity);
    }

    function _paid(uint16 quantity) internal pure returns (uint128) {
        return uint128(_protocol(quantity, BASIS, CLEARING_RATE) * WCOEN_SCALE);
    }

    function _list(address a, address b) internal pure returns (address[] memory winners) {
        winners = new address[](2);
        winners[0] = a;
        winners[1] = b;
    }

    function _one(address a) internal pure returns (address[] memory winners) {
        winners = new address[](1);
        winners[0] = a;
    }

    function _apply(address[] memory winners, uint16 partialIndex, uint16 partialWon, bool completesDay)
        internal
        returns (uint128)
    {
        vm.prank(relayer);
        return
            escrow.finalizeAuction(
                DAY, RECEIVE_ID, winners, partialIndex, partialWon, CLEARING_RATE, BASIS, completesDay
            );
    }

    function test_APartialFillPaysForTheUnitsItWon() public {
        uint128 aliceLock = _lock(alice, 800_000, 1);
        uint128 bobLock = _lock(bob, 900_000, 5);

        uint128 routed = _apply(_list(alice, bob), 1, 2, true);

        assertEq(routed, _paid(1) + _paid(2), "bob pays for the two units he won");
        assertEq(token.balanceOf(proceeds), routed);
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(DAY, bob);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Won));
        assertEq(lock.quantity, 2, "the lock keeps the units actually won");

        (uint128 owed,) = escrow.getClaimableRefund(DAY, bob);
        assertEq(owed, bobLock - _paid(2));
        (owed,) = escrow.getClaimableRefund(DAY, alice);
        assertEq(owed, aliceLock - _paid(1));
    }

    function test_APartialFillCoveringTheWholeBidIsSkipped() public {
        _lock(bob, 900_000, 5);

        vm.expectEmit(true, true, true, true, address(escrow));
        emit IEscrowAdapter.BidderRefundFailed(
            RECEIVE_ID, DAY, bob, abi.encodeWithSelector(IEscrowAdapter.PartialFillNotPartial.selector, 5, 5)
        );
        uint128 routed = _apply(_one(bob), 0, 5, true);

        assertEq(routed, 0);
        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(DAY, bob);
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.Locked), "left whole");
        assertEq(lock.quantity, 5);
    }

    function test_APartialIndexMeansNothingWithoutAPartialFill() public {
        _lock(alice, 800_000, 1);
        _lock(bob, 900_000, 5);

        uint128 routed = _apply(_list(alice, bob), 1, 0, true);

        assertEq(routed, _paid(1) + _paid(5), "bob pays for every unit he bid for");
        assertEq(escrow.getBidLock(DAY, bob).quantity, 5);
    }

    function test_AWinnerNamedTwicePaysOnce() public {
        _lock(alice, 800_000, 3);

        vm.expectEmit(true, true, true, true, address(escrow));
        emit IEscrowAdapter.BidderRefundFailed(
            RECEIVE_ID, DAY, alice, abi.encodeWithSelector(IEscrowAdapter.LockNotActive.selector)
        );
        uint128 routed = _apply(_list(alice, alice), 0, 0, false);
        assertEq(routed, _paid(3));

        routed = _apply(_one(alice), 0, 0, true);
        assertEq(routed, 0, "a later chunk cannot take the payment again");
        assertEq(token.balanceOf(proceeds), _paid(3));
    }

    function test_TheFirstChunkWithWinnersRecordsTheTermsTheRestMustMatch() public {
        _lock(alice, 800_000, 1);
        _lock(bob, 800_000, 1);
        _apply(_one(alice), 0, 0, false);

        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ClearingTermsMismatch.selector, CLEARING_RATE + 1, BASIS));
        escrow.finalizeAuction(DAY, RECEIVE_ID, _one(bob), 0, 0, CLEARING_RATE + 1, BASIS, true);

        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ClearingTermsMismatch.selector, CLEARING_RATE, BASIS + 1));
        escrow.finalizeAuction(DAY, RECEIVE_ID, _one(bob), 0, 0, CLEARING_RATE, BASIS + 1, true);

        assertEq(_apply(_one(bob), 0, 0, true), _paid(1), "the same terms apply");
    }

    function test_APartialFillOutsideTheChunkIsRejected() public {
        _lock(alice, 800_000, 3);

        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.PartialFillOutsideChunk.selector, uint16(1), uint256(1)));
        escrow.finalizeAuction(DAY, RECEIVE_ID, _one(alice), 1, 2, CLEARING_RATE, BASIS, true);

        assertEq(uint8(escrow.getBidLock(DAY, alice).status), uint8(IEscrowAdapter.LockStatus.Locked), "left whole");
    }

    function test_AChunkWithWinnersNeedsClearingTerms() public {
        _lock(alice, 800_000, 1);

        vm.prank(relayer);
        vm.expectRevert(IEscrowAdapter.ClearingTermsMissing.selector);
        escrow.finalizeAuction(DAY, RECEIVE_ID, _one(alice), 0, 0, 0, BASIS, true);

        vm.prank(relayer);
        vm.expectRevert(IEscrowAdapter.ClearingTermsMissing.selector);
        escrow.finalizeAuction(DAY, RECEIVE_ID, _one(alice), 0, 0, CLEARING_RATE, 0, true);
    }

    function test_AnEmptyChunkRecordsNoTerms() public {
        _lock(alice, 800_000, 1);

        vm.prank(relayer);
        escrow.finalizeAuction(DAY, RECEIVE_ID, new address[](0), 0, 0, CLEARING_RATE + 1, BASIS + 1, false);

        assertEq(_apply(_one(alice), 0, 0, true), _paid(1), "the first chunk with winners sets the terms");
    }

    function test_ALoserOnADayStillOpenWaitsForTheDelay() public {
        _lock(alice, 800_000, 1);
        uint128 bobLock = _lock(bob, 500_000, 1);
        _apply(_one(alice), 0, 0, false);

        (uint128 owed, uint32 claimableAt) = escrow.getClaimableRefund(DAY, bob);
        assertEq(owed, bobLock);
        assertEq(claimableAt, uint32(block.timestamp) + escrow.UNFINALIZED_REFUND_DELAY());
        vm.expectRevert(
            abi.encodeWithSelector(IEscrowAdapter.RefundNotYetClaimable.selector, claimableAt, uint32(block.timestamp))
        );
        _refund(DAY, bob);
    }

    function _close(address who) internal returns (uint16) {
        vm.prank(auction);
        return escrow.closeWin(DAY, who);
    }

    function test_ClosingAWinPaysTheChangeAndReturnsTheUnits() public {
        uint128 aliceLock = _lock(alice, 800_000, 3);
        _apply(_one(alice), 0, 0, true);

        assertEq(_close(alice), 3, "the units won");
        assertEq(token.balanceOf(alice), aliceLock - _paid(3), "the change goes with them");
        assertEq(uint8(escrow.getBidLock(DAY, alice).status), uint8(IEscrowAdapter.LockStatus.None), "lock gone");
        (,, uint128 totalLocked) = escrow.getAuctionStatus(DAY);
        assertEq(totalLocked, 0);
    }

    function test_APartialWinnerClosesWithTheUnitsItWon() public {
        _lock(alice, 800_000, 1);
        _lock(bob, 900_000, 5);
        _apply(_list(alice, bob), 1, 2, true);

        assertEq(_close(bob), 2);
    }

    function test_AWinRefundedFirstClosesWithoutAnotherWithdrawal() public {
        uint128 aliceLock = _lock(alice, 800_000, 3);
        _apply(_one(alice), 0, 0, true);
        _refund(DAY, alice);
        assertEq(token.balanceOf(alice), aliceLock - _paid(3), "change refunded");
        assertEq(escrow.getBidLock(DAY, alice).quantity, 3, "the units still wait");

        vm.expectCall(address(compact), abi.encodeWithSelector(MockTheCompact.forcedWithdrawal.selector), 0);
        assertEq(_close(alice), 3);
        assertEq(token.balanceOf(alice), aliceLock - _paid(3), "nothing paid twice");
    }

    function test_AnExactlyPaidWinClosesWithoutAWithdrawal() public {
        _lock(alice, uint32(CLEARING_RATE), 2);
        _apply(_one(alice), 0, 0, true);

        vm.expectCall(address(compact), abi.encodeWithSelector(MockTheCompact.forcedWithdrawal.selector), 0);
        assertEq(_close(alice), 2);
    }

    function test_AWinnerWithNoChangeHasNothingToRefund() public {
        _lock(alice, 800_000, 3);
        _apply(_one(alice), 0, 0, true);
        _refund(DAY, alice);

        vm.expectRevert(IEscrowAdapter.NothingToRefund.selector);
        _refund(DAY, alice);
    }

    function test_AClosedWinCannotCloseOrRefundAgain() public {
        _lock(alice, 800_000, 3);
        _apply(_one(alice), 0, 0, true);
        _close(alice);

        vm.prank(auction);
        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.closeWin(DAY, alice);

        uint256 balance = token.balanceOf(alice);
        vm.prank(auction);
        (bool closed, bool finalized) = escrow.releaseRefund(DAY, alice);
        assertTrue(closed && finalized, "a gone lock on a finalized day reports closed");
        assertEq(token.balanceOf(alice), balance, "and pays nothing");
    }

    function test_AGoneLockOnAnOpenDayHasNothingToRelease() public {
        _lock(alice, 800_000, 1);
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());
        _refund(DAY, alice);

        vm.prank(auction);
        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.releaseRefund(DAY, alice);
    }

    function test_OnlyTheAuctionReleasesARefund() public {
        _lock(alice, 800_000, 1);
        _apply(new address[](0), 0, 0, true);

        vm.prank(alice);
        vm.expectRevert();
        escrow.releaseRefund(DAY, alice);
    }

    function test_TheValveWaitsForItsDelayAndPaysOnlyMoney() public {
        uint128 aliceLock = _lock(alice, 800_000, 3);
        uint128 bobLock = _lock(bob, 500_000, 1);
        _apply(_one(alice), 0, 0, true);

        uint32 claimableAt = uint32(block.timestamp) + escrow.REFUND_ABANDON_DELAY();
        vm.expectRevert(
            abi.encodeWithSelector(IEscrowAdapter.RefundNotYetAbandoned.selector, claimableAt, uint32(block.timestamp))
        );
        escrow.claimAbandonedRefund(DAY, alice);

        vm.warp(claimableAt);
        escrow.claimAbandonedRefund(DAY, alice);
        escrow.claimAbandonedRefund(DAY, bob);
        assertEq(token.balanceOf(alice), aliceLock - _paid(3), "the winner's change");
        assertEq(token.balanceOf(bob), bobLock, "the loser's principal");
        assertEq(escrow.getBidLock(DAY, alice).quantity, 3, "the units stay for the auction");
        assertEq(uint8(escrow.getBidLock(DAY, bob).status), uint8(IEscrowAdapter.LockStatus.None));

        vm.expectRevert(IEscrowAdapter.NothingToRefund.selector);
        escrow.claimAbandonedRefund(DAY, alice);
        assertEq(_close(alice), 3, "the win still closes");
    }

    function test_OnlyTheAuctionClosesAWin() public {
        _lock(alice, 800_000, 3);
        _apply(_one(alice), 0, 0, true);

        vm.prank(alice);
        vm.expectRevert();
        escrow.closeWin(DAY, alice);
    }

    /// @dev Only a payment the escrow took makes a win. Every winner the chunk skipped has none.
    function test_AWinnerTheEscrowCouldNotChargeHasNoWin() public {
        address carol = address(7);
        _lock(alice, 500_000, 1); // bid below the clearing rate: the payment exceeds the lock
        _lock(bob, 900_000, 5); // a partial fill as large as the bid
        _lock(carol, 800_000, 1); // refunded in full before the chunk arrives
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());
        _refund(DAY, carol);

        address[] memory winners = new address[](3);
        winners[0] = alice;
        winners[1] = bob;
        winners[2] = carol;
        assertEq(_apply(winners, 1, 5, true), 0, "nobody paid");

        for (uint256 i = 0; i < winners.length; i++) {
            vm.prank(auction);
            vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
            escrow.closeWin(DAY, winners[i]);
        }
    }

    /// @dev The same vectors pin the clearing side's `rate_lock`.
    function test_TheLockMatchesTheClearingSide() public {
        uint256[4][6] memory vectors = [
            [uint256(1), 1, 999_999, 0],
            [uint256(3), 333_333, 1, 0],
            [uint256(7), 123_456_789, 987_654, 853_528_140],
            [uint256(2), 1_500_001, 333_333, 999_999],
            [uint256(40), 99_999_999, 600_001, 2_400_003_975],
            [uint256(65_535), 100_000e6, 1_000_000, 6_553_500_000_000_000]
        ];
        for (uint256 i = 0; i < vectors.length; ++i) {
            (uint16 quantity, uint128 basis, uint32 rate) =
                (uint16(vectors[i][0]), uint128(vectors[i][1]), uint32(vectors[i][2]));
            address who = address(uint160(0xF00 + i));
            uint256 expected = vectors[i][3] * WCOEN_SCALE;
            token.mint(who, expected);
            vm.prank(who);
            token.approve(address(escrow), type(uint256).max);
            vm.prank(auction);
            if (expected == 0) vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.ZeroValue.selector, "amount"));
            escrow.lockFunds(DAY, who, basis, rate, quantity);
            if (expected != 0) assertEq(escrow.getBidLock(DAY, who).lockedAmount, expected);
        }
    }
}
