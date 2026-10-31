// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote} from "../interfaces/IERC7786.sol";
import {IMailbox, IMessageRecipient} from "../interfaces/IHyperlane.sol";
import {GasLimitAttribute} from "../libs/GasLimitAttribute.sol";

/**
 * @dev ERC-7786 gateway adapter for Hyperlane.
 *
 * Wraps a Hyperlane `Mailbox` behind the ERC-7786 `IERC7786GatewaySource` interface. A
 * protocol-agnostic facade (e.g. {ERC7786Bridge}) can then route messages through Hyperlane without
 * knowing about Hyperlane domains or routers. All Hyperlane-specific concerns live here:
 *
 * * chainId <-> Hyperlane domain equivalence,
 * * remote router (the matching adapter on the destination chain) registration,
 * * native fee payment via `msg.value` (Mailbox.quoteDispatch / dispatch).
 *
 * IMPORTANT: unlike LayerZero's OApp, a bare Hyperlane Mailbox does NOT verify the message sender
 * against a trusted peer. It delivers from any sender that passes the ISM. This adapter therefore
 * enforces the peer check itself in {handle}: the message must originate from the registered remote
 * router for its origin domain.
 *
 * NOTE: EVM chains only. This adapter sets the destination-execution gas per message via Hyperlane
 * hook metadata (the executionGasLimit attribute, or `defaultGasLimit` when absent).
 *
 * UUPS upgradeable behind an ERC1967 proxy. The mailbox is an implementation immutable, so every
 * upgrade must pass the same mailbox to the constructor.
 */
contract HyperlaneGatewayAdapter is
    IERC7786GatewaySource,
    IGatewayQuote,
    IMessageRecipient,
    OwnableUpgradeable,
    UUPSUpgradeable
{
    using InteroperableAddress for bytes;

    /// @dev The local Hyperlane mailbox (sends outbound, delivers inbound).
    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    IMailbox public immutable MAILBOX;

    /// @custom:storage-location erc7201:outbe.crosschain.HyperlaneGatewayAdapter
    struct HyperlaneGatewayAdapterStorage {
        /// @dev ERC-7930 EVM chainId => Hyperlane domain. Zero means "not registered".
        mapping(uint256 chainId => uint32 domain) chainIdToDomain;
        /// @dev Hyperlane domain => ERC-7930 EVM chainId (reverse of {chainIdToDomain}).
        mapping(uint32 domain => uint256 chainId) domainToChainId;
        /// @dev Hyperlane domain => remote adapter (the trusted peer on that chain), as a Hyperlane
        ///      bytes32 address.
        mapping(uint32 domain => bytes32 router) routers;
        /// @dev Destination execution gas used when a message carries no executionGasLimit attribute.
        uint128 defaultGasLimit;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.crosschain.HyperlaneGatewayAdapter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x317567f253c6b996f17b78ed54095160db78f9375bfe2d8513005d5ee0527f00;

    /// @dev Variant selector of Hyperlane's StandardHookMetadata (the gas/value override format).
    uint16 private constant HOOK_METADATA_VARIANT = 1;

    event RouterRegistered(uint256 indexed chainId, uint32 indexed domain, bytes32 router);
    event MessageReceived(uint32 indexed origin, bytes32 indexed sender, bytes payload);
    event DefaultGasLimitUpdated(uint128 gasLimit);

    error UnknownDestinationChain(uint256 chainId);
    error RemoteRouterNotSet(uint32 domain);
    error UnauthorizedCaller(address caller);
    error UnauthorizedSender(uint32 origin, bytes32 sender);
    error RecipientExecutionFailed();

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address mailbox_) {
        MAILBOX = IMailbox(mailbox_);
        _disableInitializers();
    }

    function initialize(address owner_) external initializer {
        __Ownable_init(owner_);
        _s().defaultGasLimit = 200_000;
    }

    function _authorizeUpgrade(address) internal override onlyOwner {}

    function _s() private pure returns (HyperlaneGatewayAdapterStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    // =================================================== Getters ===================================================

    function chainIdToDomain(uint256 chainId) public view returns (uint32) {
        return _s().chainIdToDomain[chainId];
    }

    function domainToChainId(uint32 domain) public view returns (uint256) {
        return _s().domainToChainId[domain];
    }

    function routers(uint32 domain) public view returns (bytes32) {
        return _s().routers[domain];
    }

    function defaultGasLimit() public view returns (uint128) {
        return _s().defaultGasLimit;
    }

    // =================================================== Config ====================================================

    /**
     * @dev Registers the remote adapter (`router`) for a Hyperlane `domain` and binds that domain
     * to an EVM `chainId`.
     */
    function setRouterWithChain(uint32 domain, bytes32 router, uint256 chainId) public virtual onlyOwner {
        HyperlaneGatewayAdapterStorage storage $ = _s();
        $.routers[domain] = router;
        $.chainIdToDomain[chainId] = domain;
        $.domainToChainId[domain] = chainId;
        emit RouterRegistered(chainId, domain, router);
    }

    function setDefaultGasLimit(uint128 gasLimit) public virtual onlyOwner {
        _s().defaultGasLimit = gasLimit;
        emit DefaultGasLimitUpdated(gasLimit);
    }

    // ============================================ IERC7786GatewaySource ============================================

    /// @inheritdoc IERC7786GatewaySource
    function supportsAttribute(bytes4 selector) public pure virtual returns (bool) {
        return selector == GasLimitAttribute.SELECTOR;
    }

    /// @inheritdoc IERC7786GatewaySource
    function sendMessage(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        public
        payable
        virtual
        returns (bytes32)
    {
        (uint32 domain, bytes32 remoteRouter) = _route(recipient);

        // Carry the source sender and the final recipient so the remote adapter can deliver per
        // ERC-7786.
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes memory adapterPayload = abi.encode(sender, recipient, payload);

        bytes memory metadata = _metadata(GasLimitAttribute.resolve(attributes, defaultGasLimit()));

        // msg.value funds the Hyperlane native fee.
        MAILBOX.dispatch{value: msg.value}(domain, remoteRouter, adapterPayload, metadata);

        emit MessageSent(bytes32(0), sender, recipient, payload, msg.value, attributes);
        return bytes32(0);
    }

    /// @inheritdoc IGatewayQuote
    /// @dev Quotes the Hyperlane native fee using `defaultGasLimit` for destination execution.
    function quote(bytes calldata recipient, bytes calldata payload) public view virtual returns (uint256 nativeFee) {
        return _quoteWithGas(recipient, payload, defaultGasLimit());
    }

    /// @dev Quotes the native fee. Takes the destination gas from the executionGasLimit attribute
    /// (or `defaultGasLimit` when absent), so the estimate matches {sendMessage}.
    function quote(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        public
        view
        virtual
        returns (uint256 nativeFee)
    {
        return _quoteWithGas(recipient, payload, GasLimitAttribute.resolve(attributes, defaultGasLimit()));
    }

    // =============================================== Hyperlane inbound ==============================================

    /// @inheritdoc IMessageRecipient
    function handle(uint32 origin, bytes32 sender, bytes calldata message) external payable virtual {
        // Hyperlane has no built-in peer check: enforce both the mailbox and the trusted remote
        // router here.
        require(msg.sender == address(MAILBOX), UnauthorizedCaller(msg.sender));
        bytes32 expected = _s().routers[origin];
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
        HyperlaneGatewayAdapterStorage storage $ = _s();
        domain = $.chainIdToDomain[chainId];
        require(domain != 0, UnknownDestinationChain(chainId));
        remoteRouter = $.routers[domain];
        require(remoteRouter != bytes32(0), RemoteRouterNotSet(domain));
    }

    /// @dev Hyperlane StandardHookMetadata that overrides the destination gas limit (refunds excess
    ///      to the caller).
    ///      Layout: variant | msgValue(0) | gasLimit | refundAddress.
    function _metadata(uint128 gasLimit) private view returns (bytes memory) {
        return abi.encodePacked(HOOK_METADATA_VARIANT, uint256(0), uint256(gasLimit), msg.sender);
    }

    function _quoteWithGas(bytes calldata recipient, bytes calldata payload, uint128 gasLimit)
        private
        view
        returns (uint256)
    {
        (uint32 domain, bytes32 remoteRouter) = _route(recipient);
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes memory adapterPayload = abi.encode(sender, recipient, payload);
        return MAILBOX.quoteDispatch(domain, remoteRouter, adapterPayload, _metadata(gasLimit));
    }
}
