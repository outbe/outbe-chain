// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IFidelity {
    /// Encrypted owner authorization and encrypted Fidelity receipt. The source
    /// identity, query timestamp and response stay inside the private envelope.
    function query(bytes calldata encryptedRequest) external view returns (bytes memory encryptedReceipt);

    /// Returns fidelity index decimals precision.
    function decimals() external view returns (uint8);

    /// Synthetic maximum Fidelity Index (saturating RCFI) at `timestamp`.
    /// Derived from the plaintext global anchor - no authorization needed.
    function maxFidelityIndexAt(uint64 timestamp) external view returns (uint256);

    /// Lowest league (inclusive).
    function minLeague() external view returns (uint16);

    /// Highest league (inclusive).
    function maxLeague() external view returns (uint16);
}
