// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {OApp, Origin, MessagingFee} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/OApp.sol";
import {OptionsBuilder} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oapp/libs/OptionsBuilder.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote} from "../interfaces/IERC7786.sol";

/**
 * @dev ERC-7786 gateway adapter for LayerZero V2.
 *
 * Wraps a LayerZero `OApp` behind the ERC-7786 `IERC7786GatewaySource` interface so that a protocol-agnostic facade
 * (e.g. {ERC7786Router}) can route messages through LayerZero without knowing anything about endpoint ids or peers.
 * All LayerZero-specific concerns live here:
 *
 * * chainId <-> LayerZero endpoint id (eid) equivalence,
 * * peer registration (the matching adapter on the remote chain) and the inbound peer check (inherited from OApp),
 * * native fee payment via `msg.value` and destination gas options.
 *
 * Outbound: {sendMessage} resolves the recipient's chainId to an eid and `_lzSend`s the wrapped package. Inbound:
 * {_lzReceive} unwraps the package and forwards it to the ERC-7786 recipient encoded in the message.
 *
 * NOTE: EVM chains only. The ERC-7930 recipient is parsed as an EVM v1 interoperable address.
 */
contract LayerZeroGatewayAdapter is OApp, IERC7786GatewaySource, IGatewayQuote {
    using OptionsBuilder for bytes;
    using InteroperableAddress for bytes;

    /// @dev ERC-7930 EVM chainId => LayerZero endpoint id. Zero means "not registered".
    mapping(uint256 chainId => uint32 eid) public chainIdToEid;

    /// @dev LayerZero endpoint id => ERC-7930 EVM chainId (reverse of {chainIdToEid}).
    mapping(uint32 eid => uint256 chainId) public eidToChainId;

    /// @dev Gas granted to the recipient's execution on the destination chain.
    uint128 public defaultGasLimit;

    event ChainRegistered(uint256 indexed chainId, uint32 indexed eid, bytes32 peer);
    event DefaultGasLimitUpdated(uint128 gasLimit);
    event MessageReceived(uint32 indexed srcEid, bytes32 indexed sender, bytes payload);

    error UnknownDestinationChain(uint256 chainId);
    error RecipientExecutionFailed();

    constructor(address endpoint_, address owner_) OApp(endpoint_, owner_) Ownable(owner_) {
        defaultGasLimit = 200_000;
    }

    // =================================================== Config ====================================================

    /**
     * @dev Registers the remote adapter (`peer`) for a LayerZero `eid` and binds that `eid` to an EVM `chainId`.
     * Mirrors LayerZero's peer model while adding the chainId equivalence the ERC-7786 layer needs.
     */
    function setPeerWithChain(uint32 eid, bytes32 peer, uint256 chainId) public virtual onlyOwner {
        _setPeer(eid, peer);
        chainIdToEid[chainId] = eid;
        eidToChainId[eid] = chainId;
        emit ChainRegistered(chainId, eid, peer);
    }

    function setDefaultGasLimit(uint128 gasLimit) public virtual onlyOwner {
        defaultGasLimit = gasLimit;
        emit DefaultGasLimitUpdated(gasLimit);
    }

    // ============================================ IERC7786GatewaySource ============================================

    /// @inheritdoc IERC7786GatewaySource
    function supportsAttribute(
        bytes4 /*selector*/
    )
        public
        pure
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
        returns (bytes32)
    {
        // Use of `if () revert` syntax to avoid accessing attributes[0] if it's empty.
        if (attributes.length > 0) {
            revert UnsupportedAttribute(attributes[0].length < 0x04 ? bytes4(0) : bytes4(attributes[0][0:4]));
        }

        uint32 dstEid = _eidForRecipient(recipient);

        // Carry the source sender and the final recipient so the remote adapter can deliver per ERC-7786.
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes memory adapterPayload = abi.encode(sender, recipient, payload);
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(defaultGasLimit, 0);

        // msg.value funds the LayerZero native fee; excess is refunded to the caller (the facade).
        _lzSend(dstEid, adapterPayload, options, MessagingFee(msg.value, 0), payable(msg.sender));

        emit MessageSent(bytes32(0), sender, recipient, payload, msg.value, attributes);
        return bytes32(0);
    }

    /// @inheritdoc IGatewayQuote
    /// @dev Quotes the LayerZero native fee for delivering `payload` to `recipient`.
    function quote(bytes calldata recipient, bytes calldata payload) public view virtual returns (uint256 nativeFee) {
        uint32 dstEid = _eidForRecipient(recipient);
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes memory adapterPayload = abi.encode(sender, recipient, payload);
        bytes memory options = OptionsBuilder.newOptions().addExecutorLzReceiveOption(defaultGasLimit, 0);
        return _quote(dstEid, adapterPayload, options, false).nativeFee;
    }

    // ============================================== LayerZero inbound ==============================================

    function _lzReceive(
        Origin calldata origin,
        bytes32 guid,
        bytes calldata message,
        address,
        /*executor*/
        bytes calldata /*extraData*/
    )
        internal
        virtual
        override
    {
        // OApp has already verified `origin.sender == peers[origin.srcEid]` (the trusted remote adapter).
        emit MessageReceived(origin.srcEid, origin.sender, message);

        (bytes memory sender, bytes memory recipient, bytes memory payload) = abi.decode(message, (bytes, bytes, bytes));

        (, address target) = recipient.parseEvmV1();
        bytes4 result = IERC7786Recipient(target).receiveMessage(guid, sender, payload);
        require(result == IERC7786Recipient.receiveMessage.selector, RecipientExecutionFailed());
    }

    // ================================================== Internal ===================================================

    function _eidForRecipient(bytes calldata recipient) internal view virtual returns (uint32 dstEid) {
        (uint256 chainId,) = recipient.parseEvmV1Calldata();
        dstEid = chainIdToEid[chainId];
        require(dstEid != 0, UnknownDestinationChain(chainId));
    }
}
