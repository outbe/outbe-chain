// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @notice This interface declares the shared event for status=0 synthetic receipts. It has no callable functions.
interface IOutbeFailure {
    /// @notice Match the stable subsystem code. Do not match the human-readable reason.
    /// @dev Zero-fee policy codes 100-199 use the zero-fee policy log address.
    ///      Soft system-transaction codes 200-299 use the system-transaction address.
    ///      Reason text can change between releases. This declaration preserves the existing event encoding.
    event OutbeFailure(uint16 indexed code, string reason);
}
