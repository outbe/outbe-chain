// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IPromisFactory — Promis mint/burn orchestration entry point (0x2337).
/// @notice Thin orchestration layer on top of the Promis token (0x1337). Mint
///         orchestration (`mine`) is an internal cross-module API used by
///         GemFactory and IntexFactory — it wraps `Promis.mine`, records the
///         Fidelity acquisition cohort, and emits `PromisMined`. The only
///         user-facing entry point is `mineCoen`, the symmetric sale path that
///         burns Promis, records the Fidelity sale cohort, and mints native
///         COEN 1:1.
interface IPromisFactory {
    /// @notice Emitted when promis is minted to `account` (e.g. via GemFactory's
    ///         or IntexFactory's mine path). The Fidelity acquisition cohort is
    ///         recorded in the same call.
    event PromisMined(address indexed account, uint256 amount);

    /// @notice Emitted when `sender` converts promis to native COEN via
    ///         `mineCoen`. The promis is burned, the Fidelity sale cohort is
    ///         recorded, and `unitAmount` native COEN is minted 1:1.
    event CoenMined(address indexed sender, uint256 promisAmount, uint256 unitAmount);

    /// @notice Convert `amount` promis to native COEN at 1:1. Burns the promis,
    ///         records the Fidelity sale cohort, and mints the native COEN to
    ///         `msg.sender`. Returns the minted native amount.
    function mineCoen(uint256 amount) external returns (uint256);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
