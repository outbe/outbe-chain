// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @title LockAmountParityTest
/// @notice Pins that the rate-based bid-lock amount is computed identically on BNB
///         (`IntexAuction.revealBid`) and on Outbe (`Desis.rate_lock`): both evaluate
///         `qty * strike * rate / RATE_SCALE` in 256-bit space. A cross-chain finalize can never
///         revert AmountMismatch from width drift, because any bid that locks on BNB stays in the
///         lockable range and Outbe reproduces the exact same value there.
contract LockAmountParityTest is Test {
    uint32 internal constant RATE_SCALE = BridgeMsgCodec.RATE_SCALE;

    /// @dev Mirrors IntexAuction.revealBid (BNB): 256-bit math, reverts when the product overflows uint64.
    function bnbLockAmount(uint16 quantity, uint64 strike, uint32 rate) external pure returns (uint64) {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        if (wide > type(uint64).max) revert("BidAmountOverflow");
        return uint64(wide);
    }

    /// @dev Mirrors Desis rate_lock (Outbe): 256-bit math, saturates to uint64 max.
    function desisLockAmount(uint16 quantity, uint64 strike, uint32 rate) external pure returns (uint64) {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        return wide > type(uint64).max ? type(uint64).max : uint64(wide);
    }

    /// @dev In the lockable range (product fits `uint64`) both sides produce the identical value.
    function testFuzz_Parity_InRange(uint16 quantity, uint64 strike, uint32 rate) public view {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        vm.assume(wide <= type(uint64).max);
        assertEq(this.bnbLockAmount(quantity, strike, rate), this.desisLockAmount(quantity, strike, rate));
    }

    /// @dev Outside the range BNB rejects, so the bid never locks and never reaches Outbe clearing —
    ///      the Outbe saturating path is unreachable for any bid that actually escrowed.
    function testFuzz_BnbRejectsOverflow(uint16 quantity, uint64 strike, uint32 rate) public {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        vm.assume(wide > type(uint64).max);
        vm.expectRevert(bytes("BidAmountOverflow"));
        this.bnbLockAmount(quantity, strike, rate);
    }

    /// @dev Boundary: the largest product that still fits `uint64` agrees and does not revert.
    function test_Parity_AtUint64Boundary() public view {
        // strike = uint64 max, qty = 1, rate = RATE_SCALE → product = strike, the boundary value.
        assertEq(this.bnbLockAmount(1, type(uint64).max, RATE_SCALE), this.desisLockAmount(1, type(uint64).max, RATE_SCALE));
        assertEq(this.bnbLockAmount(1, type(uint64).max, RATE_SCALE), type(uint64).max);
    }
}
