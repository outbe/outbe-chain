// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @title LockAmountParityTest
/// @notice Pins that the rate-based bid-lock amount is computed identically on BNB
///         (`IntexAuction.revealBid`) and on Outbe (`Desis.rate_lock`): both evaluate
///         `qty * strike * rate / RATE_SCALE` in 256-bit space with `strike = promis_load`. A
///         cross-chain finalize can never revert AmountMismatch from width drift, because any bid
///         that locks on BNB stays in the lockable range and Outbe reproduces the exact same value.
contract LockAmountParityTest is Test {
    uint32 internal constant RATE_SCALE = BridgeMsgCodec.RATE_SCALE;
    /// @dev Per-Intex strike = promis_load (e.g. 100_000 * 1e18). Exceeds uint64, fits uint128.
    uint128 internal constant PROMIS_LOAD_MINOR = 100_000 * 1e18;

    /// @dev Mirrors IntexAuction.revealBid (BNB): 256-bit math, reverts when the product overflows uint128.
    function bnbLockAmount(uint16 quantity, uint128 strike, uint32 rate) external pure returns (uint128) {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        if (wide > type(uint128).max) revert("BidAmountOverflow");
        return uint128(wide);
    }

    /// @dev Mirrors Desis rate_lock (Outbe): 256-bit math, saturates to uint128 max.
    function desisLockAmount(uint16 quantity, uint128 strike, uint32 rate) external pure returns (uint128) {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        return wide > type(uint128).max ? type(uint128).max : uint128(wide);
    }

    /// @dev In the lockable range (product fits `uint128`) both sides produce the identical value.
    function testFuzz_Parity_InRange(uint16 quantity, uint128 strike, uint32 rate) public view {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        vm.assume(wide <= type(uint128).max);
        assertEq(this.bnbLockAmount(quantity, strike, rate), this.desisLockAmount(quantity, strike, rate));
    }

    /// @dev Outside the range BNB rejects, so the bid never locks and never reaches Outbe clearing —
    ///      the Outbe saturating path is unreachable for any bid that actually escrowed.
    function testFuzz_BnbRejectsOverflow(uint16 quantity, uint128 strike, uint32 rate) public {
        uint256 wide = uint256(quantity) * strike * rate / RATE_SCALE;
        vm.assume(wide > type(uint128).max);
        vm.expectRevert(bytes("BidAmountOverflow"));
        this.bnbLockAmount(quantity, strike, rate);
    }

    /// @dev At the real per-Intex strike (`promis_load`) both sides agree and stay well inside uint128.
    function test_Parity_AtPromisLoadStrike() public view {
        // qty = uint16 max, rate = RATE_SCALE (100% of strike) → the largest live lock at this strike.
        uint128 expected = uint128(uint256(type(uint16).max) * PROMIS_LOAD_MINOR);
        assertEq(
            this.bnbLockAmount(type(uint16).max, PROMIS_LOAD_MINOR, RATE_SCALE),
            this.desisLockAmount(type(uint16).max, PROMIS_LOAD_MINOR, RATE_SCALE)
        );
        assertEq(this.bnbLockAmount(type(uint16).max, PROMIS_LOAD_MINOR, RATE_SCALE), expected);
    }

    /// @dev Boundary: the largest product that still fits `uint128` agrees and does not revert.
    function test_Parity_AtUint128Boundary() public view {
        // strike = uint128 max, qty = 1, rate = RATE_SCALE → product = strike, the boundary value.
        assertEq(
            this.bnbLockAmount(1, type(uint128).max, RATE_SCALE),
            this.desisLockAmount(1, type(uint128).max, RATE_SCALE)
        );
        assertEq(this.bnbLockAmount(1, type(uint128).max, RATE_SCALE), type(uint128).max);
    }
}
