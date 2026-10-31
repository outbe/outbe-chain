// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {IERC7786GatewaySource, IERC7786Recipient, IGatewayQuote} from "../interfaces/IERC7786.sol";
import {GasLimitAttribute} from "../libs/GasLimitAttribute.sol";

/**
 * @dev ERC-7786 gateway adapter for same-chain delivery.
 *
 * When the destination chain is the local chain, no external transport is needed. {sendMessage}
 * hands the wrapped package straight back to the hub's {IERC7786Recipient-receiveMessage} in the
 * same transaction, for zero fee. The executionGasLimit attribute bounds the delivery, and a
 * try/catch isolates it, mirroring a real transport. A revert on the receiving side parks the
 * delivery for a permissionless {retryDelivery} instead of rolling back the send.
 *
 * Wiring: the hub registers itself as the remote bridge for the local chain and sets this adapter
 * as the local chain's gateway. Rotating the local gateway strands parked deliveries (the hub stops
 * trusting this adapter), so parked entries must be drained first.
 *
 * UUPS upgradeable behind an ERC1967 proxy. The hub is an implementation immutable, so every
 * upgrade must pass the same hub to the constructor.
 */
contract LoopbackGatewayAdapter is OwnableUpgradeable, UUPSUpgradeable, IERC7786GatewaySource, IGatewayQuote {
    using InteroperableAddress for bytes;

    struct ParkedDelivery {
        address target;
        bool done;
        bytes sender;
        bytes payload;
    }

    /// @dev The ERC7786Bridge hub: the only allowed caller and the delivery target of every message.
    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    address public immutable HUB;

    /// @custom:storage-location erc7201:outbe.crosschain.LoopbackGatewayAdapter
    struct LoopbackGatewayAdapterStorage {
        /// @dev Gas granted to the delivery when the executionGasLimit attribute is absent.
        uint128 defaultGasLimit;
        /// @dev Failed deliveries awaiting {retryDelivery}.
        mapping(uint256 idx => ParkedDelivery delivery) parked;
        uint256 nextParkedIdx;
        /// @dev Distinguishes receive ids of otherwise identical messages (the hub deduplicates on its
        ///      own nonce).
        uint256 nonce;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.crosschain.LoopbackGatewayAdapter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xca52c5fd7a28cdd3903145ce69611eb734334650fc7f1dce512323e6db96c400;

    event DefaultGasLimitUpdated(uint128 gasLimit);
    event DeliveryParked(uint256 indexed idx, bytes reason);
    event DeliveryRetried(uint256 indexed idx);

    error OnlyHub(address caller);
    error NonZeroValue();
    error NotLocalChain(uint256 chainId);
    error InsufficientForwardGas(uint256 gasLimit);
    error NoParkedDelivery(uint256 idx);
    error AlreadyDelivered(uint256 idx);
    error RecipientExecutionFailed();

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address hub_) {
        HUB = hub_;
        _disableInitializers();
    }

    function initialize(address owner_) external initializer {
        __Ownable_init(owner_);
        _s().defaultGasLimit = 200_000;
    }

    function _authorizeUpgrade(address) internal override onlyOwner {}

    function _s() private pure returns (LoopbackGatewayAdapterStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    // =================================================== Getters ===================================================

    function defaultGasLimit() public view returns (uint128) {
        return _s().defaultGasLimit;
    }

    function parked(uint256 idx)
        public
        view
        returns (address target, bool done, bytes memory sender, bytes memory payload)
    {
        ParkedDelivery storage delivery = _s().parked[idx];
        return (delivery.target, delivery.done, delivery.sender, delivery.payload);
    }

    function nextParkedIdx() public view returns (uint256) {
        return _s().nextParkedIdx;
    }

    // =================================================== Config ====================================================

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
    /// @dev Delivers immediately with the attribute-resolved gas. On a delivery revert, the adapter
    /// parks the message, and the send still succeeds (failure isolation, like an asynchronous
    /// transport).
    function sendMessage(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        public
        payable
        virtual
        returns (bytes32)
    {
        require(msg.sender == HUB, OnlyHub(msg.sender));
        require(msg.value == 0, NonZeroValue());

        (uint256 chainId, address target) = recipient.parseEvmV1Calldata();
        require(chainId == block.chainid, NotLocalChain(chainId));

        uint128 gasLimit = GasLimitAttribute.resolve(attributes, defaultGasLimit());
        bytes memory sender = InteroperableAddress.formatEvmV1(block.chainid, msg.sender);
        bytes32 receiveId = keccak256(abi.encode(address(this), ++_s().nonce));

        uint256 gasBefore = gasleft();
        try IERC7786Recipient(target).receiveMessage{gas: gasLimit}(receiveId, sender, payload) returns (bytes4 magic) {
            // The target is the hub, which returns the magic value or reverts. Never park a
            // non-reverting call.
            require(magic == IERC7786Recipient.receiveMessage.selector, RecipientExecutionFailed());
        } catch (bytes memory reason) {
            // Park only a delivery that provably received the full gas limit. The check covers
            // EIP-150 headroom plus the call's argument encoding, which scales with the payload.
            // An under-gassed send reverts outright, so a delivery can never be falsely parked.
            require(
                gasBefore >= uint256(gasLimit) + uint256(gasLimit) / 63 + 5_000 + payload.length / 4,
                InsufficientForwardGas(gasLimit)
            );
            LoopbackGatewayAdapterStorage storage $ = _s();
            uint256 idx = $.nextParkedIdx++;
            $.parked[idx] = ParkedDelivery({target: target, done: false, sender: sender, payload: payload});
            emit DeliveryParked(idx, reason);
        }

        emit MessageSent(bytes32(0), sender, recipient, payload, 0, attributes);
        return bytes32(0);
    }

    // ============================================== IGatewayQuote ==================================================

    /// @inheritdoc IGatewayQuote
    function quote(bytes calldata, bytes calldata) public pure virtual returns (uint256) {
        return 0;
    }

    /// @inheritdoc IGatewayQuote
    function quote(bytes calldata, bytes calldata, bytes[] calldata) public pure virtual returns (uint256) {
        return 0;
    }

    // ================================================== Retry ======================================================

    /// @dev Re-attempts a parked delivery with the transaction's full gas. Permissionless. A revert
    /// rolls the whole call back, and the delivery stays parked and retryable.
    function retryDelivery(uint256 idx) public virtual {
        ParkedDelivery storage delivery = _s().parked[idx];
        require(delivery.target != address(0), NoParkedDelivery(idx));
        require(!delivery.done, AlreadyDelivered(idx));
        delivery.done = true;

        bytes4 magic = IERC7786Recipient(delivery.target)
            .receiveMessage(keccak256(abi.encode(address(this), idx)), delivery.sender, delivery.payload);
        require(magic == IERC7786Recipient.receiveMessage.selector, RecipientExecutionFailed());
        emit DeliveryRetried(idx);
    }
}
