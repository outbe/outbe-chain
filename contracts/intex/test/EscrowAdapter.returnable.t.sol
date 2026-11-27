// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {DeployProxy} from "./utils/DeployProxy.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev `returnableAmount` must report exactly what `returnIntexAuctionAsset` then pays, from exactly when
///      it pays it.
contract EscrowAdapterReturnableTest is Test {
    EscrowAdapter internal escrow;
    MockTheCompact internal compact;
    MockWCOEN internal token;

    address internal admin = address(1);
    address internal relayer = address(2);
    address internal auction = address(3);
    address internal bidder = address(5);
    address internal other = address(6);

    uint32 internal constant DAY = 1;
    uint128 internal constant LOCK_UNITS = 1000e6;
    // MockWCOEN has 18 decimals: 1e12 token units per protocol unit.
    uint128 internal constant LOCK = LOCK_UNITS * 1e12;
    /// @dev One Intex at the full rate locks exactly `LOCK`.
    uint128 internal constant BASIS = 1000e6;

    function setUp() public {
        compact = new MockTheCompact();
        token = new MockWCOEN();
        compact.setResetPeriodSeconds(0);
        escrow = DeployProxy.escrowAdapter(admin, auction, relayer, address(compact), address(token));

        _lock(bidder);
        _lock(other);
    }

    function _refund(uint32 emissionDay, address bidder) internal {
        vm.prank(auction);
        escrow.returnIntexAuctionAsset(emissionDay, bidder);
    }

    function _lock(address who) internal {
        token.mint(who, LOCK);
        vm.prank(who);
        token.approve(address(escrow), type(uint256).max);
        vm.prank(auction);
        escrow.lockIntexAuctionAsset(DAY, who, LOCK_UNITS, 1_000_000, 1);
    }

    function _finalize(address[] memory winners, uint64 clearingRate, bool completesDay) internal {
        vm.prank(relayer);
        escrow.chargeIntexAuctionAsset(DAY, winners, 0, 0, clearingRate, BASIS, completesDay);
    }

    function _only(address who) internal pure returns (address[] memory winners) {
        winners = new address[](1);
        winners[0] = who;
    }

    /// @dev The claim pays the view's amount in full, not a wei earlier than its timestamp, and
    ///      leaves nothing after.
    function _assertClaimMatchesView(uint128 expectedAmount, uint32 expectedAt) internal {
        (uint128 amount, uint32 returnableAt) = escrow.returnableAmount(DAY, bidder);
        assertEq(amount, expectedAmount, "amount");
        assertEq(returnableAt, expectedAt, "returnable at");

        if (returnableAt != 0) {
            vm.warp(returnableAt - 1);
            vm.expectRevert(
                abi.encodeWithSelector(
                    IEscrowAdapter.IntexAuctionAssetNotYetReturnable.selector, returnableAt, returnableAt - 1
                )
            );
            _refund(DAY, bidder);
            vm.warp(returnableAt);
        }

        uint256 before = token.balanceOf(bidder);
        _refund(DAY, bidder);
        assertEq(token.balanceOf(bidder) - before, amount, "the claim pays what the view reported");

        (amount, returnableAt) = escrow.returnableAmount(DAY, bidder);
        assertEq(amount, 0, "nothing left");
        assertEq(returnableAt, 0, "no date either");
    }

    function test_NothingToClaimWithoutALock() public view {
        (uint128 amount, uint32 returnableAt) = escrow.returnableAmount(DAY, address(0xDEAD));
        assertEq(amount, 0);
        assertEq(returnableAt, 0);
    }

    function test_ADayThatNeverFinalizedOwesThePrincipal() public {
        _assertClaimMatchesView(LOCK, uint32(block.timestamp) + escrow.UNFINALIZED_RETURN_DELAY());
    }

    function test_ABidderLeftOutOfAFinalizedDayOwesThePrincipalAtOnce() public {
        _finalize(_only(other), 1_000_000, true);
        _assertClaimMatchesView(LOCK, 0);
    }

    function test_AWinnerOwesTheRestOfItsLockAtOnce() public {
        _finalize(_only(bidder), 400_000, true);
        _assertClaimMatchesView(LOCK - 400e18, 0);
    }

    function test_AWinnerDoesNotWaitForTheRestOfItsDay() public {
        _finalize(_only(bidder), 400_000, false);
        _assertClaimMatchesView(LOCK - 400e18, 0);
    }

    function test_ASettledLockHasNothingToClaim() public {
        _finalize(_only(bidder), 1_000_000, true);
        (uint128 amount, uint32 returnableAt) = escrow.returnableAmount(DAY, bidder);
        assertEq(amount, 0);
        assertEq(returnableAt, 0);
    }

    function test_AClaimDeletesTheLock() public {
        vm.warp(block.timestamp + escrow.UNFINALIZED_RETURN_DELAY());
        _refund(DAY, bidder);

        IEscrowAdapter.IntexAuctionBidLock memory lock = escrow.getIntexAuctionBidLock(DAY, bidder);
        assertEq(lock.lockedAmount, 0, "amount cleared");
        assertEq(lock.lockedAt, 0, "time cleared");
        assertEq(uint8(lock.status), uint8(IEscrowAdapter.LockStatus.None), "status cleared");

        vm.expectRevert(IEscrowAdapter.LockNotActive.selector);
        _refund(DAY, bidder);
    }
}
