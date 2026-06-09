// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";

/// @title LockAmountParityTest
/// @notice Pins that the bid-lock amount is computed identically on BNB (uint64 `quantity*bidPrice`)
///         and on Outbe (uint256 `quantity*bidPrice` bounded to uint64), so a cross-chain finalize
///         can never revert AmountMismatch from width drift.
contract LockAmountParityTest is Test {
    /// @dev Mirrors IntexAuction.revealBid (BNB): uint64 math, reverts on overflow.
    function bnbLockAmount(uint16 quantity, uint64 bidPrice) external pure returns (uint64) {
        return uint64(quantity) * bidPrice;
    }

    /// @dev Mirrors Desis._calculateClearing (Outbe): uint256 math, bounded to uint64.
    function desisLockAmount(uint16 quantity, uint64 bidPrice) external pure returns (uint64) {
        uint256 wide = uint256(quantity) * bidPrice;
        if (wide > type(uint64).max) revert("LockedAmountOverflow");
        return uint64(wide);
    }

    /// @dev In the lockable range (product fits `uint64`) both sides produce the identical value.
    function testFuzz_Parity_InRange(uint16 quantity, uint64 bidPrice) public view {
        uint256 wide = uint256(quantity) * bidPrice;
        vm.assume(wide <= type(uint64).max);
        assertEq(this.bnbLockAmount(quantity, bidPrice), this.desisLockAmount(quantity, bidPrice));
    }

    /// @dev Outside the range BOTH sides reject — no `(quantity, bidPrice)` locks on BNB but is
    ///      rejected by Desis (or vice-versa), so no width-drift `AmountMismatch` can arise.
    function testFuzz_Parity_BothRejectOverflow(uint16 quantity, uint64 bidPrice) public {
        uint256 wide = uint256(quantity) * bidPrice;
        vm.assume(wide > type(uint64).max);
        vm.expectRevert(); // BNB `uint64` multiply overflows → Panic(0x11); the bid never locks.
        this.bnbLockAmount(quantity, bidPrice);
        vm.expectRevert(bytes("LockedAmountOverflow")); // Desis bound rejects the same input.
        this.desisLockAmount(quantity, bidPrice);
    }

    /// @dev Boundary: the largest product that still fits `uint64` agrees and does not revert.
    function test_Parity_AtUint64Boundary() public view {
        assertEq(this.bnbLockAmount(1, type(uint64).max), this.desisLockAmount(1, type(uint64).max));
        assertEq(this.bnbLockAmount(1, type(uint64).max), type(uint64).max);
    }
}
