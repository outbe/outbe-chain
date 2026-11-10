// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

/**
 * @dev Minimal Hyperlane core interfaces used by the Outbe crosschainbridge precompile, vendored
 * locally like {IHyperlane}. This file declares only the owner-side subset. Selectors match
 * Hyperlane core (hyperlane-xyz/core) 11.3.1.
 */
interface IInterchainAccountRouter {
    struct Call {
        bytes32 to;
        uint256 value;
        bytes data;
    }

    /// @dev Dispatches `_calls` for the caller's Interchain Account on `_destination` to execute.
    /// The caller must supply the IGP fee (see {quoteGasPayment}) as `msg.value`.
    function callRemote(uint32 _destination, Call[] calldata _calls) external payable returns (bytes32);

    /// @dev Native fee required by {callRemote} for `_destination`.
    function quoteGasPayment(uint32 _destination) external view returns (uint256);

    /// @dev Interchain Account of `_owner` on `_destination`: the account that executes the
    /// calls of {callRemote}.
    function getRemoteInterchainAccount(uint32 _destination, address _owner) external view returns (address);
}

/// @dev StorageMessageIdMultisigIsm: validators and threshold live in storage. It is Ownable2Step,
/// so `transferOwnership` only stages `pendingOwner`, and the new owner must call
/// {acceptOwnership}.
interface IStorageMultisigIsm {
    function setValidatorsAndThreshold(address[] calldata _validators, uint8 _threshold) external;

    /// @dev Current set. The storage variant ignores the message argument.
    function validatorsAndThreshold(bytes calldata _message) external view returns (address[] memory, uint8);

    function acceptOwnership() external;

    function pendingOwner() external view returns (address);
}

interface IOwnable {
    function owner() external view returns (address);
}

/// @dev Remote-router table of Hyperlane's `Router`, which the InterchainAccountRouter extends.
interface IHyperlaneRouter {
    function routers(uint32 _domain) external view returns (bytes32);

    function enrollRemoteRouter(uint32 _domain, bytes32 _router) external;
}

/// @dev Owner setter of Hyperlane's `InterchainGasPaymaster`: the gas oracle and the gas overhead
/// of each destination domain.
interface IInterchainGasPaymaster {
    struct DomainGasConfig {
        address gasOracle;
        uint96 gasOverhead;
    }

    struct GasParam {
        uint32 remoteDomain;
        DomainGasConfig config;
    }

    function setDestinationGasConfigs(GasParam[] calldata _configs) external;
}

/// @dev Owner setter of Hyperlane's `StorageGasOracle`: the exchange rate and the gas price of each
/// remote domain.
interface IStorageGasOracle {
    struct RemoteGasDataConfig {
        uint32 remoteDomain;
        uint128 tokenExchangeRate;
        uint128 gasPrice;
    }

    function setRemoteGasDataConfigs(RemoteGasDataConfig[] calldata _configs) external;
}
