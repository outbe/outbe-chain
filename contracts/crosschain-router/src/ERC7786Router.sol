// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote} from "./interfaces/IERC7786.sol";

/**
 * @dev 1-of-1 ERC-7786 router.
 *
 * A thin facade that decouples application contracts from any specific cross-chain protocol. Applications talk to this
 * router through the stable ERC-7786 interface; the concrete protocol lives behind a swappable gateway adapter. To
 * switch protocols (e.g. LayerZero -> Hyperlane) the owner points the router at a different adapter via
 * {setActiveGateway} -- application contracts never change.
 *
 * Unlike a N-of-M aggregator, this router uses a single active gateway:
 *
 * * outbound: {sendMessage} forwards through {activeGateway} only;
 * * inbound: {receiveMessage} is accepted only when the caller is {activeGateway} (the trusted adapter on this chain)
 *   AND the cross-chain sender is the bridge's registered counterpart on the source chain.
 *
 * NOTE: switching the active gateway drops messages that are already in flight through the previous adapter: they are
 * rejected on arrival ({UnauthorizedGateway}) and can never be delivered through the old adapter again. Recovery is by
 * design left to the source: the application re-sends the message, which now travels through the new active gateway.
 * This is safe against double-execution precisely because the in-flight message is permanently rejected and therefore
 * never executes -- the re-sent message carries a fresh nonce and executes exactly once.
 */
contract ERC7786Router is IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote, Ownable, Pausable {
    using InteroperableAddress for bytes;

    /// @dev The single active gateway adapter: used for outbound sends and trusted for inbound delivery.
    address private _activeGateway;

    /// @dev Address of the matching router on a given chain (keyed by ERC-7930 chain type and reference).
    mapping(bytes2 chainType => mapping(bytes chainReference => bytes addr)) private _remotes;

    /// @dev Tracks messages that have already been delivered to their recipient, to prevent re-execution.
    mapping(bytes32 id => bool executed) private _executed;

    /// @dev Nonce ensuring uniqueness of the wrapped payload (and therefore of the message id).
    uint256 private _nonce;

    event GatewayUpdated(address indexed gateway);
    event RemoteRegistered(bytes remote);
    event MessageForwarded(bytes32 indexed id, address indexed recipient);

    error NoActiveGateway();
    error UnauthorizedGateway(address gateway);
    error InvalidCrosschainSender();
    error RemoteNotRegistered(bytes2 chainType, bytes chainReference);
    error AlreadyExecuted();
    error InvalidExecutionReturnValue();

    constructor(address owner_, address gateway_) Ownable(owner_) {
        if (gateway_ != address(0)) _setActiveGateway(gateway_);
    }

    // ============================================ IERC7786GatewaySource ============================================

    /// @inheritdoc IERC7786GatewaySource
    function supportsAttribute(
        bytes4 /*selector*/
    )
        public
        view
        virtual
        returns (bool)
    {
        return false;
    }

    /// @inheritdoc IERC7786GatewaySource
    function sendMessage(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        public
        payable
        virtual
        whenNotPaused
        returns (bytes32 sendId)
    {
        // Use of `if () revert` syntax to avoid accessing attributes[0] if it's empty.
        if (attributes.length > 0) {
            revert UnsupportedAttribute(attributes[0].length < 0x04 ? bytes4(0) : bytes4(attributes[0][0:4]));
        }

        address gateway = _activeGateway;
        require(gateway != address(0), NoActiveGateway());

        // Address of the remote router on the destination chain, reverts if not registered.
        bytes memory remote = getRemoteRouter(recipient);
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);

        // Wrap payload so the remote router can recover the original sender and final recipient.
        bytes memory wrappedPayload = abi.encode(++_nonce, sender, recipient, payload);

        // Forward any native value to the gateway: fee-bearing transports (LayerZero, Hyperlane, ...) charge a
        // per-message native fee that must be funded here. The gateway adapter consumes it to pay the protocol.
        bytes32 id = IERC7786GatewaySource(gateway).sendMessage{value: msg.value}(remote, wrappedPayload, attributes);
        sendId = id == bytes32(0) ? bytes32(0) : keccak256(abi.encode(gateway, id));

        emit MessageSent(sendId, sender, recipient, payload, msg.value, attributes);
    }

    // ================================================ IGatewayQuote ================================================

    /// @inheritdoc IGatewayQuote
    /// @dev Quotes the native fee that {sendMessage} would require for the same `recipient` and `payload`, by
    /// delegating to the active gateway. The payload is wrapped exactly as in {sendMessage} so the quoted message
    /// size matches what is actually sent (the nonce value does not affect the encoded length).
    function quote(bytes calldata recipient, bytes calldata payload) public view virtual returns (uint256 nativeFee) {
        address gateway = _activeGateway;
        require(gateway != address(0), NoActiveGateway());

        bytes memory remote = getRemoteRouter(recipient);
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes memory wrappedPayload = abi.encode(_nonce + 1, sender, recipient, payload);

        return IGatewayQuote(gateway).quote(remote, wrappedPayload);
    }

    // ============================================== IERC7786Recipient ==============================================

    /// @inheritdoc IERC7786Recipient
    function receiveMessage(
        bytes32,
        /*receiveId*/
        bytes calldata sender,
        bytes calldata payload
    )
        public
        payable
        virtual
        whenNotPaused
        returns (bytes4)
    {
        // Variant B: only the currently active gateway adapter may deliver messages.
        require(msg.sender == _activeGateway, UnauthorizedGateway(msg.sender));
        // The cross-chain sender must be the registered counterpart router on the source chain.
        require(keccak256(getRemoteRouter(sender)) == keccak256(sender), InvalidCrosschainSender());

        (, bytes memory originalSender, bytes memory recipient, bytes memory unwrappedPayload) =
            abi.decode(payload, (uint256, bytes, bytes, bytes));

        // Deduplicate. The id binds the source router (sender) and the wrapped payload (which carries the nonce).
        bytes32 id = keccak256(abi.encode(sender, payload));
        require(!_executed[id], AlreadyExecuted());
        // Effects before interaction (CEI). If the recipient call reverts, the whole tx reverts and this flag is
        // rolled back, leaving the message retryable on redelivery.
        _executed[id] = true;

        (, address target) = recipient.parseEvmV1();
        bytes4 magic = IERC7786Recipient(target).receiveMessage(id, originalSender, unwrappedPayload);
        require(magic == IERC7786Recipient.receiveMessage.selector, InvalidExecutionReturnValue());

        emit MessageForwarded(id, target);
        return IERC7786Recipient.receiveMessage.selector;
    }

    // =================================================== Getters ===================================================

    function activeGateway() public view virtual returns (address) {
        return _activeGateway;
    }

    function getRemoteRouter(bytes memory chain) public view virtual returns (bytes memory) {
        (bytes2 chainType, bytes memory chainReference,) = chain.parseV1();
        return getRemoteRouter(chainType, chainReference);
    }

    function getRemoteRouter(bytes2 chainType, bytes memory chainReference) public view virtual returns (bytes memory) {
        bytes memory addr = _remotes[chainType][chainReference];
        require(addr.length != 0, RemoteNotRegistered(chainType, chainReference));
        return InteroperableAddress.formatV1(chainType, chainReference, addr);
    }

    // =================================================== Setters ===================================================

    function setActiveGateway(address gateway) public virtual onlyOwner {
        _setActiveGateway(gateway);
    }

    function registerRemoteRouter(bytes calldata remote) public virtual onlyOwner {
        (bytes2 chainType, bytes calldata chainReference, bytes calldata addr) = remote.parseV1Calldata();
        _remotes[chainType][chainReference] = addr;
        emit RemoteRegistered(remote);
    }

    function pause() public virtual onlyOwner {
        _pause();
    }

    function unpause() public virtual onlyOwner {
        _unpause();
    }

    // ================================================== Internal ===================================================

    function _setActiveGateway(address gateway) internal virtual {
        _activeGateway = gateway;
        emit GatewayUpdated(gateway);
    }
}
