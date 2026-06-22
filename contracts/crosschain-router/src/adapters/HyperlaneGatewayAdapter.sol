// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote} from "../interfaces/IERC7786.sol";
import {IMailbox, IMessageRecipient} from "../interfaces/IHyperlane.sol";

/**
 * @dev ERC-7786 gateway adapter for Hyperlane.
 *
 * Wraps a Hyperlane `Mailbox` behind the ERC-7786 `IERC7786GatewaySource` interface so a protocol-agnostic facade
 * (e.g. {ERC7786Router}) can route messages through Hyperlane without knowing about Hyperlane domains or routers.
 * All Hyperlane-specific concerns live here:
 *
 * * chainId <-> Hyperlane domain equivalence,
 * * remote router (the matching adapter on the destination chain) registration,
 * * native fee payment via `msg.value` (Mailbox.quoteDispatch / dispatch).
 *
 * IMPORTANT: unlike LayerZero's OApp, a bare Hyperlane Mailbox does NOT verify the message sender against a trusted
 * peer -- it delivers from any sender that passes the ISM. This adapter therefore enforces the peer check itself in
 * {handle}: the message must originate from the registered remote router for its origin domain.
 *
 * NOTE: EVM chains only. The destination-execution gas uses the Mailbox's default hook; per-message gas overrides
 * (via hook metadata) are intentionally out of scope for this version.
 */
contract HyperlaneGatewayAdapter is IERC7786GatewaySource, IGatewayQuote, IMessageRecipient, Ownable {
    using InteroperableAddress for bytes;

    /// @dev The local Hyperlane mailbox (sends outbound, delivers inbound).
    IMailbox public immutable MAILBOX;

    /// @dev ERC-7930 EVM chainId => Hyperlane domain. Zero means "not registered".
    mapping(uint256 chainId => uint32 domain) public chainIdToDomain;

    /// @dev Hyperlane domain => ERC-7930 EVM chainId (reverse of {chainIdToDomain}).
    mapping(uint32 domain => uint256 chainId) public domainToChainId;

    /// @dev Hyperlane domain => remote adapter (the trusted peer on that chain), as a Hyperlane bytes32 address.
    mapping(uint32 domain => bytes32 router) public routers;

    event RouterRegistered(uint256 indexed chainId, uint32 indexed domain, bytes32 router);
    event MessageReceived(uint32 indexed origin, bytes32 indexed sender, bytes payload);

    error UnknownDestinationChain(uint256 chainId);
    error RemoteRouterNotSet(uint32 domain);
    error UnauthorizedCaller(address caller);
    error UnauthorizedSender(uint32 origin, bytes32 sender);
    error RecipientExecutionFailed();

    constructor(address mailbox_, address owner_) Ownable(owner_) {
        MAILBOX = IMailbox(mailbox_);
    }

    // =================================================== Config ====================================================

    /**
     * @dev Registers the remote adapter (`router`) for a Hyperlane `domain` and binds that domain to an EVM `chainId`.
     */
    function setRouterWithChain(uint32 domain, bytes32 router, uint256 chainId) public virtual onlyOwner {
        routers[domain] = router;
        chainIdToDomain[chainId] = domain;
        domainToChainId[domain] = chainId;
        emit RouterRegistered(chainId, domain, router);
    }

    // ============================================ IERC7786GatewaySource ============================================

    /// @inheritdoc IERC7786GatewaySource
    function supportsAttribute(bytes4 /*selector*/ ) public pure virtual returns (bool) {
        return false;
    }

    /// @inheritdoc IERC7786GatewaySource
    function sendMessage(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        public
        payable
        virtual
        returns (bytes32)
    {
        // Use of `if () revert` syntax to avoid accessing attributes[0] if it's empty.
        if (attributes.length > 0) {
            revert UnsupportedAttribute(attributes[0].length < 0x04 ? bytes4(0) : bytes4(attributes[0][0:4]));
        }

        (uint32 domain, bytes32 remoteRouter) = _route(recipient);

        // Carry the source sender and the final recipient so the remote adapter can deliver per ERC-7786.
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes memory adapterPayload = abi.encode(sender, recipient, payload);

        // msg.value funds the Hyperlane native fee.
        MAILBOX.dispatch{value: msg.value}(domain, remoteRouter, adapterPayload);

        emit MessageSent(bytes32(0), sender, recipient, payload, msg.value, attributes);
        return bytes32(0);
    }

    /// @inheritdoc IGatewayQuote
    /// @dev Quotes the Hyperlane native fee for delivering `payload` to `recipient`.
    function quote(bytes calldata recipient, bytes calldata payload) public view virtual returns (uint256 nativeFee) {
        (uint32 domain, bytes32 remoteRouter) = _route(recipient);
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes memory adapterPayload = abi.encode(sender, recipient, payload);
        return MAILBOX.quoteDispatch(domain, remoteRouter, adapterPayload);
    }

    // =============================================== Hyperlane inbound ==============================================

    /// @inheritdoc IMessageRecipient
    function handle(uint32 origin, bytes32 sender, bytes calldata message) external payable virtual {
        // Hyperlane has no built-in peer check: enforce both the mailbox and the trusted remote router here.
        require(msg.sender == address(MAILBOX), UnauthorizedCaller(msg.sender));
        bytes32 expected = routers[origin];
        require(expected != bytes32(0) && sender == expected, UnauthorizedSender(origin, sender));

        emit MessageReceived(origin, sender, message);

        (bytes memory innerSender, bytes memory recipient, bytes memory payload) =
            abi.decode(message, (bytes, bytes, bytes));

        bytes32 receiveId = keccak256(abi.encode(origin, sender, message));
        (, address target) = recipient.parseEvmV1();
        bytes4 result = IERC7786Recipient(target).receiveMessage(receiveId, innerSender, payload);
        require(result == IERC7786Recipient.receiveMessage.selector, RecipientExecutionFailed());
    }

    // ================================================== Internal ===================================================

    function _route(bytes calldata recipient) internal view virtual returns (uint32 domain, bytes32 remoteRouter) {
        (uint256 chainId,) = recipient.parseEvmV1Calldata();
        domain = chainIdToDomain[chainId];
        require(domain != 0, UnknownDestinationChain(chainId));
        remoteRouter = routers[domain];
        require(remoteRouter != bytes32(0), RemoteRouterNotSet(domain));
    }
}
