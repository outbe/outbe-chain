// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IntexUnits} from "@contracts/shared/libs/IntexUnits.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev A refund chunk names a day's winners and its clearing terms; the escrow works out each payment itself.
contract EscrowAdapterWinnersTest is Test {
    EscrowAdapter internal escrow;
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
        MockTheCompact compact = new MockTheCompact();
        compact.setResetPeriodSeconds(0);
        token = new MockWCOEN();

        vm.startPrank(admin);
        escrow.wire(auction, address(compact), address(token));
        escrow.setProceedsRecipient(proceeds);
        vm.stopPrank();
    }

    function _lock(address who, uint32 bidRate, uint16 quantity) internal returns (uint128 amount) {
        amount = uint128(IntexUnits.escrowAmount(quantity, BASIS, bidRate));
        token.mint(who, amount);
        vm.prank(who);
        token.approve(address(escrow), type(uint256).max);
        vm.prank(auction);
        escrow.lockFunds(DAY, who, amount, bidRate, quantity);
    }

    function _paid(uint16 quantity) internal pure returns (uint128) {
        return uint128(IntexUnits.escrowAmount(quantity, BASIS, CLEARING_RATE));
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

    /// @dev The same vectors pin the clearing side's `rate_lock`.
    function test_TheFormulaMatchesTheClearingSide() public pure {
        assertEq(IntexUnits.escrowAmount(1, 1, 999_999), 0);
        assertEq(IntexUnits.escrowAmount(3, 333_333, 1), 0);
        assertEq(IntexUnits.escrowAmount(7, 123_456_789, 987_654), 853_528_140e12);
        assertEq(IntexUnits.escrowAmount(2, 1_500_001, 333_333), 999_999e12);
        assertEq(IntexUnits.escrowAmount(40, 99_999_999, 600_001), 2_400_003_975e12);
        assertEq(IntexUnits.escrowAmount(65_535, 100_000e6, 1_000_000), 6_553_500_000_000_000e12);
    }
}
