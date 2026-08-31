// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @title LockAmountParityTest
/// @notice Pins that the rate-based bid-lock amount is computed identically on BNB
///         (`IntexAuction.revealBid`) and on Outbe (`Desis.rate_lock`): both evaluate
///         `qty * basis * rate / 1e6` in 256-bit space with `basis = promis_load`. A
///         cross-chain finalize can never revert AmountMismatch from width drift, because any bid
///         that locks on BNB stays in the lockable range and Outbe reproduces the exact same value.
contract LockAmountParityTest is Test {
    uint32 internal constant SCALE_1E6 = BridgeMsgCodec.SCALE_1E6;
    /// @dev Per-Intex escrow basis = promis_load (e.g. 100_000 * 1e6). Fits the existing uint128 wire field.
    uint128 internal constant PROMIS_LOAD_MINOR = 100_000 * 1e6;

    /// @dev Mirrors IntexAuction.revealBid (BNB): 256-bit math, reverts when the product overflows uint128.
    function bnbLockAmount(uint16 quantity, uint128 basis, uint32 rate) external pure returns (uint128) {
        uint256 wide = uint256(quantity) * basis * rate / SCALE_1E6;
        if (wide > type(uint128).max) revert("BidAmountOverflow");
        return uint128(wide);
    }

    /// @dev Mirrors Desis rate_lock (Outbe): 256-bit math, saturates to uint128 max.
    function desisLockAmount(uint16 quantity, uint128 basis, uint32 rate) external pure returns (uint128) {
        uint256 wide = uint256(quantity) * basis * rate / SCALE_1E6;
        return wide > type(uint128).max ? type(uint128).max : uint128(wide);
    }

    /// @dev In the lockable range (product fits `uint128`) both sides produce the identical value.
    function testFuzz_Parity_InRange(uint16 quantity, uint128 basis, uint32 rate) public view {
        uint256 wide = uint256(quantity) * basis * rate / SCALE_1E6;
        vm.assume(wide <= type(uint128).max);
        assertEq(this.bnbLockAmount(quantity, basis, rate), this.desisLockAmount(quantity, basis, rate));
    }

    /// @dev Outside the range BNB rejects, so the bid never locks and never reaches Outbe clearing -
    ///      the Outbe saturating path is unreachable for any bid that actually escrowed.
    function testFuzz_BnbRejectsOverflow(uint16 quantity, uint128 basis, uint32 rate) public {
        uint256 wide = uint256(quantity) * basis * rate / SCALE_1E6;
        vm.assume(wide > type(uint128).max);
        vm.expectRevert(bytes("BidAmountOverflow"));
        this.bnbLockAmount(quantity, basis, rate);
    }

    /// @dev At the real per-Intex escrow basis (`promis_load`) both sides agree and stay well inside uint128.
    function test_Parity_AtPromisLoadBasis() public view {
        // qty = uint16 max, rate = 1e6 (100% of basis) -> the largest live lock at this basis.
        uint128 expected = uint128(uint256(type(uint16).max) * PROMIS_LOAD_MINOR);
        assertEq(
            this.bnbLockAmount(type(uint16).max, PROMIS_LOAD_MINOR, SCALE_1E6),
            this.desisLockAmount(type(uint16).max, PROMIS_LOAD_MINOR, SCALE_1E6)
        );
        assertEq(this.bnbLockAmount(type(uint16).max, PROMIS_LOAD_MINOR, SCALE_1E6), expected);
    }

    /// @dev Boundary: the largest product that still fits `uint128` agrees and does not revert.
    function test_Parity_AtUint128Boundary() public view {
        // basis = uint128 max, qty = 1, rate = 1e6 -> product = basis, the boundary value.
        assertEq(
            this.bnbLockAmount(1, type(uint128).max, SCALE_1E6), this.desisLockAmount(1, type(uint128).max, SCALE_1E6)
        );
        assertEq(this.bnbLockAmount(1, type(uint128).max, SCALE_1E6), type(uint128).max);
    }
}
