// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title IntexUnits
/// @author Outbe
/// @notice The escrow amount formula, shared by the lock taken at reveal and the payment a winner makes.
library IntexUnits {
    /// @dev Carried here, rather than read from the codec, so the escrow needs no codec import.
    uint256 internal constant SCALE_1E6 = 1_000_000;
    uint256 internal constant NATIVE_UNITS_PER_PROTOCOL_UNIT = 1e12;

    /// @notice `quantity` Intexes at `rate` of the six-decimal `basis`, in 18-decimal payment units. Mirrors the
    ///         clearing side's `rate_lock` bit for bit: the six-decimal product is floored before it is scaled.
    function escrowAmount(uint256 quantity, uint256 basis, uint256 rate) internal pure returns (uint256) {
        return quantity * basis * rate / SCALE_1E6 * NATIVE_UNITS_PER_PROTOCOL_UNIT;
    }
}
