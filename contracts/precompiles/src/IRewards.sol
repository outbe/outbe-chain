// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice Native fee accounting events only; this interface has no callable methods.
/// @dev All monetary amounts are native COEN minor units (wei; 10^18 per COEN).
/// One six-decimal protocol unit equals 10^12 native units.
interface IRewards {
    event BlockFeeEscrowed(bytes32 indexed fbHash, uint64 indexed fbNumber, uint256 feeSumNative);
    event VoterFeeTransferred(bytes32 indexed fbHash, address indexed voter, uint256 amountNative);
    /// @dev transferredNative plus restNative equals the escrow. The rest goes to the shared
    /// function of the Promis Limit. Its NativeBurnedToPromisLimit event reports the burn.
    event BlockFeesTransferred(bytes32 indexed fbHash, uint256 transferredNative, uint256 restNative);
}
