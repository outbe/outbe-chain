// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev A refund chunk names a day's winners and its clearing terms. The escrow computes each
///      payment itself.
contract EscrowAdapterWinnersTest is Test {
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

    function _lock(address who, uint32 bidRate, uint16 quantity) internal returns (uint128 amount) {
        amount = uint128(BridgeMsgCodec.escrowAmount(quantity, BASIS, bidRate));
        token.mint(who, amount);
        vm.prank(who);
        token.approve(address(escrow), type(uint256).max);
        vm.prank(auction);
        escrow.lockFunds(DAY, who, amount, bidRate, quantity);
    }

    function _paid(uint16 quantity) internal pure returns (uint128) {
        return uint128(BridgeMsgCodec.escrowAmount(quantity, BASIS, CLEARING_RATE));
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
        escrow.claimRefund(DAY, bob);
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
        escrow.claimRefund(DAY, alice);
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
        escrow.claimRefund(DAY, alice);

        vm.expectRevert(IEscrowAdapter.NothingToRefund.selector);
        escrow.claimRefund(DAY, alice);
    }

    function test_AClosedWinCannotCloseOrRefundAgain() public {
        _lock(alice, 800_000, 3);
        _apply(_one(alice), 0, 0, true);
        _close(alice);

        vm.prank(auction);
        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.closeWin(DAY, alice);
        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(DAY, alice);
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
        escrow.claimRefund(DAY, carol);

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
    function test_TheFormulaMatchesTheClearingSide() public pure {
        assertEq(BridgeMsgCodec.escrowAmount(1, 1, 999_999), 0);
        assertEq(BridgeMsgCodec.escrowAmount(3, 333_333, 1), 0);
        assertEq(BridgeMsgCodec.escrowAmount(7, 123_456_789, 987_654), 853_528_140e12);
        assertEq(BridgeMsgCodec.escrowAmount(2, 1_500_001, 333_333), 999_999e12);
        assertEq(BridgeMsgCodec.escrowAmount(40, 99_999_999, 600_001), 2_400_003_975e12);
        assertEq(BridgeMsgCodec.escrowAmount(65_535, 100_000e6, 1_000_000), 6_553_500_000_000_000e12);
    }
}
