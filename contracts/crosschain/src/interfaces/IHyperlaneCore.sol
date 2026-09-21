// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

/**
 * @dev Minimal Hyperlane core interfaces used by the Outbe hyperlanecontroller precompile, vendored locally like
 * {IHyperlane}. Only the owner-side subset is declared; selectors match @hyperlane-xyz/core 11.3.1.
 */
interface IInterchainAccountRouter {
    struct Call {
        bytes32 to;
        uint256 value;
        bytes data;
    }

    /// @dev Dispatches `_calls` to be executed by the caller's Interchain Account on `_destination`. The IGP fee
    /// (see {quoteGasPayment}) must be supplied as `msg.value`.
    function callRemote(uint32 _destination, Call[] calldata _calls) external payable returns (bytes32);

    /// @dev Native fee required by {callRemote} for `_destination`.
    function quoteGasPayment(uint32 _destination) external view returns (uint256);
}

/// @dev StorageMessageIdMultisigIsm: validators and threshold live in storage. Ownable2Step, so `transferOwnership`
/// only stages `pendingOwner` and the new owner must call {acceptOwnership}.
interface IStorageMultisigIsm {
    function setValidatorsAndThreshold(address[] calldata _validators, uint8 _threshold) external;

    /// @dev Current set; the message argument is ignored by the storage variant.
    function validatorsAndThreshold(bytes calldata _message) external view returns (address[] memory, uint8);

    function acceptOwnership() external;

    function pendingOwner() external view returns (address);
}

interface IOwnable {
    function owner() external view returns (address);
}
