// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev `getClaimableRefund` must report exactly what `claimRefund` then pays, from exactly when it pays it.
contract EscrowAdapterClaimableTest is Test {
    EscrowAdapter internal escrow;
    MockTheCompact internal compact;
    MockWCOEN internal token;

    address internal admin = address(1);
    address internal relayer = address(2);
    address internal auction = address(3);
    address internal bidder = address(5);
    address internal other = address(6);

    uint32 internal constant DAY = 1;
    uint128 internal constant LOCK = 1000e18;

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, relayer);
        compact = new MockTheCompact();
        token = new MockWCOEN();
        compact.setResetPeriodSeconds(0);

        vm.startPrank(admin);
        escrow.wire(auction, address(compact), address(token));
        escrow.setProceedsRecipient(address(0xBEEF));
        vm.stopPrank();

        _lock(bidder);
        _lock(other);
    }

    function _lock(address who) internal {
        token.mint(who, LOCK);
        vm.prank(who);
        token.approve(address(escrow), type(uint256).max);
        vm.prank(auction);
        escrow.lockFunds(DAY, who, LOCK);
    }

    function _finalize(address who, uint128 refunded) internal {
        IEscrowAdapter.FinalizationInstruction[] memory instructions = new IEscrowAdapter.FinalizationInstruction[](1);
        instructions[0] = IEscrowAdapter.FinalizationInstruction({
            bidder: who, refundedAmount: refunded, paidAmount: LOCK - refunded
        });
        vm.prank(relayer);
        escrow.finalizeAuction(DAY, bytes32(uint256(1)), instructions, true);
    }

    function _finalizedAt() internal view returns (uint32 at) {
        (,, at,) = escrow.auctionEscrowState(DAY);
    }

    /// @dev The view's amount is paid in full, not a wei earlier than its timestamp, and nothing is left after.
    function _assertClaimMatchesView(uint128 expectedAmount, uint32 expectedAt) internal {
        (uint128 amount, uint32 claimableAt) = escrow.getClaimableRefund(DAY, bidder);
        assertEq(amount, expectedAmount, "amount");
        assertEq(claimableAt, expectedAt, "claimable at");

        vm.warp(claimableAt - 1);
        vm.expectRevert(
            abi.encodeWithSelector(IEscrowAdapter.RefundNotYetClaimable.selector, claimableAt, claimableAt - 1)
        );
        escrow.claimRefund(DAY, bidder);

        vm.warp(claimableAt);
        uint256 before = token.balanceOf(bidder);
        escrow.claimRefund(DAY, bidder);
        assertEq(token.balanceOf(bidder) - before, amount, "the claim pays what the view reported");

        (amount, claimableAt) = escrow.getClaimableRefund(DAY, bidder);
        assertEq(amount, 0, "nothing left");
        assertEq(claimableAt, 0, "no date either");
    }

    function test_NothingToClaimWithoutALock() public view {
        (uint128 amount, uint32 claimableAt) = escrow.getClaimableRefund(DAY, address(0xDEAD));
        assertEq(amount, 0);
        assertEq(claimableAt, 0);
    }

    function test_ADayThatNeverFinalizedOwesThePrincipal() public {
        _assertClaimMatchesView(LOCK, uint32(block.timestamp) + escrow.UNFINALIZED_REFUND_DELAY());
    }

    function test_ABidderLeftOutOfAFinalizedDayOwesThePrincipal() public {
        _finalize(other, LOCK);
        _assertClaimMatchesView(LOCK, _finalizedAt() + escrow.POST_FINALIZE_REFUND_DELAY());
    }

    function test_ARecordedSplitOwesTheRecordedRefund() public {
        compact.setForcedWithdrawalShouldFail(true);
        _finalize(bidder, LOCK / 4);
        compact.setForcedWithdrawalShouldFail(false);
        _assertClaimMatchesView(LOCK / 4, _finalizedAt() + escrow.POST_FINALIZE_REFUND_DELAY());
    }

    function test_ASettledLockHasNothingToClaim() public {
        _finalize(bidder, LOCK);
        (uint128 amount, uint32 claimableAt) = escrow.getClaimableRefund(DAY, bidder);
        assertEq(amount, 0);
        assertEq(claimableAt, 0);
    }

    function test_AClaimDeletesTheLock() public {
        vm.warp(block.timestamp + escrow.UNFINALIZED_REFUND_DELAY());
        escrow.claimRefund(DAY, bidder);

        IEscrowAdapter.BidLock memory lock = escrow.getBidLock(DAY, bidder);
        assertEq(lock.lockedAmount, 0, "amount cleared");
        assertEq(lock.lockedAt, 0, "time cleared");
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.None), "status cleared");

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        escrow.claimRefund(DAY, bidder);
    }
}
