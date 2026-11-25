// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

/**
 * @dev Minimal Hyperlane interfaces, vendored locally (single source of truth for cross-chain
 * interfaces, like {IERC7786}). This file declares only the subset that {HyperlaneGatewayAdapter}
 * and the outbe-hyperlane-agent binary use. Selectors match Hyperlane's core contracts, so calls
 * interoperate with the real contracts.
 *
 * See https://docs.hyperlane.xyz for the full specification.
 */
interface IMailbox {
    /// @dev Local Hyperlane domain id of this mailbox.
    function localDomain() external view returns (uint32);

    /// @dev Dispatches a message to `recipientAddress` on `destinationDomain` using the default
    /// hook. Returns the message id. The caller must supply the native fee (see {quoteDispatch}) as
    /// `msg.value`.
    function dispatch(uint32 destinationDomain, bytes32 recipientAddress, bytes calldata messageBody)
        external
        payable
        returns (bytes32 messageId);

    /// @dev Native fee required by {dispatch} for the same arguments.
    function quoteDispatch(uint32 destinationDomain, bytes32 recipientAddress, bytes calldata messageBody)
        external
        view
        returns (uint256 fee);

    /// @dev {dispatch} variant that carries post-dispatch hook `metadata` (e.g. a per-message
    /// destination gas override via StandardHookMetadata). The native fee (see the matching
    /// {quoteDispatch}) must be `msg.value`.
    function dispatch(
        uint32 destinationDomain,
        bytes32 recipientAddress,
        bytes calldata messageBody,
        bytes calldata metadata
    ) external payable returns (bytes32 messageId);

    /// @dev Native fee required by the metadata-carrying {dispatch} for the same arguments.
    function quoteDispatch(
        uint32 destinationDomain,
        bytes32 recipientAddress,
        bytes calldata messageBody,
        bytes calldata metadata
    ) external view returns (uint256 fee);

    /// @dev Emitted by {dispatch} on the origin chain. `message` is the full encoded message.
    event Dispatch(address indexed sender, uint32 indexed destination, bytes32 indexed recipient, bytes message);

    /// @dev True when the message with this id was processed on this chain.
    function delivered(bytes32 messageId) external view returns (bool);

    /// @dev The ISM that verifies messages for `recipient`.
    function recipientIsm(address recipient) external view returns (address);

    /// @dev Verifies `message` with the recipient ISM and delivers it to the recipient.
    function process(bytes calldata metadata, bytes calldata message) external payable;
}

interface IMessageRecipient {
    /// @dev The local Mailbox calls this to deliver a verified message from `_origin`/`_sender`.
    function handle(uint32 _origin, bytes32 _sender, bytes calldata _message) external payable;
}

/// @dev ValidatorAnnounce: where each validator publishes its checkpoint storage location.
interface IValidatorAnnounce {
    function getAnnouncedStorageLocations(address[] calldata validators) external view returns (string[][] memory);

    function mailbox() external view returns (address);

    function localDomain() external view returns (uint32);

    function announce(address validator, string calldata storageLocation, bytes calldata signature)
        external
        returns (bool);
}

/// @dev MerkleTreeHook: the tree of dispatched message ids.
interface IMerkleTreeHook {
    event InsertedIntoTree(bytes32 messageId, uint32 index);

    function latestCheckpoint() external view returns (bytes32 root, uint32 index);

    function mailbox() external view returns (address);

    function deployedBlock() external view returns (uint256);
}

/// @dev The ISM views that a relayer reads on the destination chain.
interface IIsm {
    function moduleType() external view returns (uint8);

    /// @dev Routing ISM: the ISM for this message.
    function route(bytes calldata message) external view returns (address);

    /// @dev Multisig ISM: the validators and the number of signatures.
    function validatorsAndThreshold(bytes calldata message)
        external
        view
        returns (address[] memory validators, uint8 threshold);
}
