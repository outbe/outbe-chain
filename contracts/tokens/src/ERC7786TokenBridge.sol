// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {IERC7786GatewaySource, IERC7786Recipient} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";
import {IERC7802} from "@openzeppelin/contracts/interfaces/draft-IERC7802.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {IGatewayQuote} from "./interfaces/IGatewayQuote.sol";

/// @title ERC7786TokenBridge
/// @notice ERC-7786 fungible token bridge supporting lock/unlock and ERC-7802 burn/mint sides.
contract ERC7786TokenBridge is Ownable, IERC7786Recipient {
    using SafeERC20 for IERC20;
    using InteroperableAddress for bytes;

    enum TokenBridgeMode {
        LockUnlock,
        BurnMint
    }

    IERC20 public immutable token;
    IERC7786GatewaySource public immutable bridge;
    TokenBridgeMode public immutable mode;

    mapping(uint32 domain => bytes recipient) public remoteBridges;

    event RemoteBridgeRegistered(uint32 indexed domain, bytes recipient);
    event CrosschainTransferSent(
        bytes32 indexed sendId, uint32 indexed destinationDomain, address indexed from, address to, uint256 amount
    );
    event CrosschainTransferReceived(
        bytes32 indexed receiveId, uint32 indexed sourceDomain, bytes from, address indexed to, uint256 amount
    );

    error InvalidToken();
    error InvalidBridge();
    error InvalidAmount();
    error RemoteBridgeNotSet(uint32 domain);
    error DomainTooLarge(uint256 chainId);
    error UnauthorizedBridge(address caller);
    error UnauthorizedRemoteBridge(bytes sender);

    constructor(address token_, address bridge_, address owner_, TokenBridgeMode mode_) Ownable(owner_) {
        if (token_ == address(0)) revert InvalidToken();
        if (bridge_ == address(0)) revert InvalidBridge();

        token = IERC20(token_);
        bridge = IERC7786GatewaySource(bridge_);
        mode = mode_;
    }

    function setRemoteBridge(uint32 domain, bytes calldata recipient) external onlyOwner {
        remoteBridges[domain] = recipient;
        emit RemoteBridgeRegistered(domain, recipient);
    }

    function quoteSend(uint32 destinationDomain, address to, uint256 amount) external view returns (uint256) {
        return _quoteSendFrom(msg.sender, destinationDomain, to, amount);
    }

    function quoteSendFrom(address from, uint32 destinationDomain, address to, uint256 amount)
        external
        view
        returns (uint256)
    {
        return _quoteSendFrom(from, destinationDomain, to, amount);
    }

    function send(uint32 destinationDomain, address to, uint256 amount) external payable returns (bytes32 sendId) {
        if (amount == 0) revert InvalidAmount();

        _onSend(msg.sender, amount);

        bytes memory payload = _encodePayload(msg.sender, to, amount);
        sendId = bridge.sendMessage{value: msg.value}(_remoteBridge(destinationDomain), payload, new bytes[](0));

        emit CrosschainTransferSent(sendId, destinationDomain, msg.sender, to, amount);
    }

    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        external
        payable
        returns (bytes4)
    {
        if (msg.sender != address(bridge)) revert UnauthorizedBridge(msg.sender);

        (uint256 srcChainId,) = sender.parseEvmV1Calldata();
        if (srcChainId > type(uint32).max) revert DomainTooLarge(srcChainId);

        uint32 sourceDomain = uint32(srcChainId);
        bytes memory expectedSender = _remoteBridge(sourceDomain);
        if (keccak256(sender) != keccak256(expectedSender)) revert UnauthorizedRemoteBridge(sender);

        (bytes memory from, address to, uint256 amount) = abi.decode(payload, (bytes, address, uint256));
        _onReceive(to, amount);

        emit CrosschainTransferReceived(receiveId, sourceDomain, from, to, amount);
        return IERC7786Recipient.receiveMessage.selector;
    }

    function _quoteSendFrom(address from, uint32 destinationDomain, address to, uint256 amount)
        internal
        view
        returns (uint256)
    {
        return IGatewayQuote(address(bridge)).quote(_remoteBridge(destinationDomain), _encodePayload(from, to, amount));
    }

    function _remoteBridge(uint32 domain) internal view returns (bytes memory recipient) {
        recipient = remoteBridges[domain];
        if (recipient.length == 0) revert RemoteBridgeNotSet(domain);
    }

    function _encodePayload(address from, address to, uint256 amount) internal view returns (bytes memory) {
        return abi.encode(InteroperableAddress.formatEvmV1(block.chainid, from), to, amount);
    }

    function _onSend(address from, uint256 amount) internal {
        if (mode == TokenBridgeMode.LockUnlock) {
            token.safeTransferFrom(from, address(this), amount);
        } else {
            IERC7802(address(token)).crosschainBurn(from, amount);
        }
    }

    function _onReceive(address to, uint256 amount) internal {
        if (mode == TokenBridgeMode.LockUnlock) {
            token.safeTransfer(to, amount);
        } else {
            IERC7802(address(token)).crosschainMint(to, amount);
        }
    }
}
