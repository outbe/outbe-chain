// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IPromisFactory — Promis mint/burn orchestration entry point (0x2337).
interface IPromisFactory {
    /// @notice Emitted when promis is minted to `account`.
    event PromisMined(address indexed account, uint256 amount);

    /// @notice Emitted when `sender` converts promis to native COEN.
    event CoenMined(address indexed sender, uint256 amount);

    /// @notice Convert `amount` promis to native COEN at 1:1.
    function mineCoen(uint256 amount) external returns (uint256);

    /// @notice ERC-165 conformance check.
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
