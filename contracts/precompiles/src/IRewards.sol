// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice Native fee accounting events only; this interface has no callable methods.
/// @dev All monetary amounts are native COEN minor units (wei; 10^18 per COEN).
/// One six-decimal protocol unit equals 10^12 native units.
interface IRewards {
    event BlockFeeEscrowed(bytes32 indexed fbHash, uint64 indexed fbNumber, uint256 feeSumNative);
    event VoterFeePaid(bytes32 indexed fbHash, address indexed voter, uint256 amountNative);
    /// @dev Distributed plus residue equals the escrow. Burn includes carried dust and
    /// can exceed this window's residue; dustAfterNative is below 10^12 native units.
    event LateWindowSettled(
        bytes32 indexed fbHash,
        uint256 distributedNative,
        uint256 residueNative,
        uint256 burnedNative,
        uint256 dustAfterNative
    );
}
