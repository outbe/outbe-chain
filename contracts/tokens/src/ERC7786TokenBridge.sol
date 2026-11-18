// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {IERC7786GatewaySource, IERC7786Recipient} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";
import {IERC7802} from "@openzeppelin/contracts/interfaces/draft-IERC7802.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {IStablecoin} from "@precompiles/IStablecoin.sol";

import {DEAD_ADDRESS} from "@shared/BurnAddress.sol";
import {IGatewayQuote} from "./interfaces/IGatewayQuote.sol";
import {IERC7786TokenReceiver} from "./interfaces/IERC7786TokenReceiver.sol";

/// @title ERC7786TokenBridge
/// @notice ERC-7786 fungible token bridge supporting lock/unlock, ERC-7802 burn/mint, and
///         issuer-gated mint/burn sides. Composed transfers (`sendAndCall`) additionally invoke an
///         ERC-1363-style receiver hook on the destination after the tokens are credited. If the
///         hook fails, the bridge credits no tokens and anyone can send them back to the source chain.
/// @dev The initializer sets the peer bridge on each remote domain. No function changes or removes
///      a peer. The owner is governance: the CrosschainBridge precompile on Outbe, and its
///      Interchain Account on other chains. The owner adds a peer for a new domain and authorizes
///      upgrades. UUPS upgradeable behind an ERC1967 proxy: the token, the hub, the mode and the
///      decimals are implementation immutables, so every upgrade must pass the same values.
contract ERC7786TokenBridge is OwnableUpgradeable, UUPSUpgradeable, ReentrancyGuardTransient, IERC7786Recipient {
    using SafeERC20 for IERC20;
    using InteroperableAddress for bytes;

    enum TokenBridgeMode {
        LockUnlock,
        BurnMint,
        IssuerBurnMint
    }

    /// @notice The bridge on a remote domain that this bridge sends to and accepts messages from.
    struct Peer {
        uint32 domain;
        address bridge;
    }

    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    IERC20 public immutable token;
    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    IERC7786GatewaySource public immutable bridge;
    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    TokenBridgeMode public immutable mode;
    /// @notice Decimals of `token`. A transfer moves raw amounts, so both sides must agree on them.
    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    uint8 public immutable tokenDecimals;

    /// @notice A composed delivery whose receiver hook failed. The bridge credited no tokens.
    struct FailedDelivery {
        /// @notice The domain of the source chain.
        uint32 sourceDomain;
        /// @notice The sender on the source chain. Zero means no record.
        address sender;
        /// @notice The receiver on this chain whose hook failed.
        address receiver;
        /// @notice The amount that the bridge did not credit.
        uint256 amount;
    }

    /// @custom:storage-location erc7201:outbe.tokens.ERC7786TokenBridge
    struct ERC7786TokenBridgeStorage {
        /// @notice The peer bridge on each remote domain. Zero means the domain is not connected.
        mapping(uint32 domain => address bridge) remoteBridges;
        /// @notice The failed composed deliveries that the bridge did not send back yet.
        mapping(bytes32 receiveId => FailedDelivery) failedDeliveries;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.tokens.ERC7786TokenBridge")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x306d6711a9c05487813f9253b20252074e596a70fbba8fc44e8ecac485b8ae00;

    event PeerAdded(uint32 indexed domain, address bridge);
    event CrosschainTransferSent(
        bytes32 indexed sendId, uint32 indexed destinationDomain, address indexed from, address to, uint256 amount
    );
    event CrosschainTransferReceived(
        bytes32 indexed receiveId, uint32 indexed sourceDomain, bytes from, address indexed to, uint256 amount
    );
    /// @notice The receiver hook failed. The bridge credited no tokens and stored a record.
    event CrosschainDeliveryFailed(
        bytes32 indexed receiveId,
        uint32 indexed sourceDomain,
        bytes from,
        address indexed to,
        uint256 amount,
        bytes extraData
    );
    /// @notice The bridge sent `amount` back to `to`, the sender on `sourceDomain`.
    event CrosschainDeliveryReclaimed(
        bytes32 indexed receiveId, bytes32 indexed sendId, uint32 indexed sourceDomain, address to, uint256 amount
    );

    error InvalidToken();
    error InvalidBridge();
    error InvalidAmount();
    error InvalidRecipient(address to);
    error EmptyExtraData();
    error InvalidTokenReceiver(address to);
    error InvalidPeer(uint32 domain);
    error RemoteBridgeNotSet(uint32 domain);
    error DomainTooLarge(uint256 chainId);
    error UnauthorizedBridge(address caller);
    error UnauthorizedToken(address caller);
    error UnauthorizedRemoteBridge(bytes sender);
    error DecimalsMismatch(uint8 sourceDecimals, uint8 localDecimals);
    error NoFailedDelivery(bytes32 receiveId);
    error NotSelf();

    /// @dev `executionGasLimit(uint256)` ERC-7786 attribute understood by the bridge hub's gateways.
    bytes4 private constant GAS_LIMIT_ATTR = bytes4(keccak256("executionGasLimit(uint256)"));

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address token_, address bridge_, TokenBridgeMode mode_) {
        if (token_ == address(0)) revert InvalidToken();
        if (bridge_ == address(0)) revert InvalidBridge();

        token = IERC20(token_);
        bridge = IERC7786GatewaySource(bridge_);
        mode = mode_;
        tokenDecimals = IERC20Metadata(token_).decimals();
        _disableInitializers();
    }

    function initialize(address owner_, Peer[] calldata peers_) external initializer {
        __Ownable_init(owner_);
        for (uint256 i = 0; i < peers_.length; i++) {
            _addPeer(peers_[i].domain, peers_[i].bridge);
        }
    }

    function _authorizeUpgrade(address) internal override onlyOwner {}

    function _s() private pure returns (ERC7786TokenBridgeStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @notice The peer bridge on `domain`. Zero means the domain is not connected.
    function remoteBridges(uint32 domain) public view returns (address) {
        return _s().remoteBridges[domain];
    }

    /// @notice The failed composed delivery for `receiveId`. A zero `sender` means no record.
    function failedDeliveries(bytes32 receiveId) external view returns (FailedDelivery memory) {
        return _s().failedDeliveries[receiveId];
    }

    /// @notice Connects a new domain. A peer that is already set cannot change.
    function addPeer(uint32 domain, address peerBridge) external onlyOwner {
        _addPeer(domain, peerBridge);
    }

    function quoteSend(uint32 destinationDomain, address to, uint256 amount, bytes calldata extraData, uint256 gasLimit)
        external
        view
        returns (uint256)
    {
        _requireRecipient(to);
        return IGatewayQuote(address(bridge))
            .quote(
                _remoteBridge(destinationDomain),
                _encodePayload(msg.sender, to, amount, extraData),
                _gasAttributes(gasLimit)
            );
    }

    function send(uint32 destinationDomain, address to, uint256 amount) external payable returns (bytes32 sendId) {
        if (amount == 0) revert InvalidAmount();
        _requireRecipient(to);

        amount = _onSend(msg.sender, amount);

        bytes memory payload = _encodePayload(msg.sender, to, amount, "");
        sendId = bridge.sendMessage{value: msg.value}(_remoteBridge(destinationDomain), payload, new bytes[](0));

        emit CrosschainTransferSent(sendId, destinationDomain, msg.sender, to, amount);
    }

    /// @notice Composed transfer: deliver `amount` to `to` and invoke its receiver hook with `extraData`.
    /// @dev The destination credits the tokens and calls the hook as one atomic step. If the hook
    ///      reverts, returns a wrong value, or `to` has no code, the destination credits no tokens.
    ///      It stores a record, and anyone can call `reclaimToSource` there to send `amount` back
    ///      to the sender on this chain as a plain transfer. The bridge does not retry the hook.
    /// @param gasLimit Destination execution gas (0 = gateway default), carried as the
    ///        `executionGasLimit` ERC-7786 attribute to cover the hook.
    function sendAndCall(
        uint32 destinationDomain,
        address to,
        uint256 amount,
        bytes calldata extraData,
        uint256 gasLimit
    ) external payable returns (bytes32 sendId) {
        if (amount == 0) revert InvalidAmount();
        _requireRecipient(to);
        if (extraData.length == 0) revert EmptyExtraData();

        amount = _onSend(msg.sender, amount);

        bytes memory payload = _encodePayload(msg.sender, to, amount, extraData);
        sendId =
            bridge.sendMessage{value: msg.value}(_remoteBridge(destinationDomain), payload, _gasAttributes(gasLimit));

        emit CrosschainTransferSent(sendId, destinationDomain, msg.sender, to, amount);
    }

    /// @notice Sends `amount`, which the token has just burned from 0x…dEaD, to 0x…dEaD on
    ///         `destinationDomain`. Only the token can call it, so no backing moves without that burn.
    function sendBurned(uint32 destinationDomain, uint256 amount) external payable returns (bytes32 sendId) {
        if (msg.sender != address(token)) revert UnauthorizedToken(msg.sender);
        if (amount == 0) revert InvalidAmount();

        bytes memory payload = _encodePayload(DEAD_ADDRESS, DEAD_ADDRESS, amount, "");
        sendId = bridge.sendMessage{value: msg.value}(_remoteBridge(destinationDomain), payload, new bytes[](0));

        emit CrosschainTransferSent(sendId, destinationDomain, DEAD_ADDRESS, DEAD_ADDRESS, amount);
    }

    function receiveMessage(bytes32 receiveId, bytes calldata sender, bytes calldata payload)
        external
        payable
        nonReentrant
        returns (bytes4)
    {
        if (msg.sender != address(bridge)) revert UnauthorizedBridge(msg.sender);

        (uint256 srcChainId, address srcBridge) = sender.parseEvmV1Calldata();
        if (srcChainId > type(uint32).max) revert DomainTooLarge(srcChainId);
        uint32 sourceDomain = uint32(srcChainId);
        if (srcBridge == address(0) || srcBridge != _s().remoteBridges[sourceDomain]) {
            revert UnauthorizedRemoteBridge(sender);
        }

        (bytes memory from, address to, uint256 amount, uint8 sourceDecimals, bytes memory extraData) =
            abi.decode(payload, (bytes, address, uint256, uint8, bytes));
        if (sourceDecimals != tokenDecimals) revert DecimalsMismatch(sourceDecimals, tokenDecimals);
        _requireRecipient(to);

        if (extraData.length == 0) {
            _onReceive(to, amount);
        } else {
            // Composed transfer. The credit and the hook run in one self-call, so a hook failure
            // rolls back the credit too. The bridge then stores a record, and the delivery
            // succeeds, so the hub does not deliver the message again.
            try this.receiveAndCall(to, sourceDomain, from, amount, extraData) {}
            catch {
                (, address sender) = from.parseEvmV1();
                _s().failedDeliveries[receiveId] = FailedDelivery(sourceDomain, sender, to, amount);
                emit CrosschainDeliveryFailed(receiveId, sourceDomain, from, to, amount, extraData);
                return IERC7786Recipient.receiveMessage.selector;
            }
        }

        emit CrosschainTransferReceived(receiveId, sourceDomain, from, to, amount);
        return IERC7786Recipient.receiveMessage.selector;
    }

    /// @notice The atomic step of a composed delivery: credits `amount` to `to` and calls its
    ///         receiver hook. Only the bridge can call it, from `receiveMessage`.
    /// @dev No `nonReentrant`: `receiveMessage` holds the lock when it calls this function.
    function receiveAndCall(
        address to,
        uint32 sourceDomain,
        bytes calldata from,
        uint256 amount,
        bytes calldata extraData
    ) external {
        if (msg.sender != address(this)) revert NotSelf();
        _onReceive(to, amount);
        bytes4 magic = IERC7786TokenReceiver(to).onCrosschainTokensReceived(sourceDomain, from, amount, extraData);
        if (magic != IERC7786TokenReceiver.onCrosschainTokensReceived.selector) revert InvalidTokenReceiver(to);
    }

    /// @notice Sends the amount of a failed composed delivery back to its sender on the source
    ///         chain, as a plain transfer. Anyone can call it. The caller pays the message fee.
    /// @dev The `from` of the return payload is the receiver whose hook failed. The bridge deletes
    ///      the record before it sends, so one delivery cannot pay on both chains.
    function reclaimToSource(bytes32 receiveId) external payable nonReentrant returns (bytes32 sendId) {
        ERC7786TokenBridgeStorage storage $ = _s();
        FailedDelivery memory failed = $.failedDeliveries[receiveId];
        if (failed.sender == address(0)) revert NoFailedDelivery(receiveId);
        delete $.failedDeliveries[receiveId];

        bytes memory payload = _encodePayload(failed.receiver, failed.sender, failed.amount, "");
        sendId = bridge.sendMessage{value: msg.value}(_remoteBridge(failed.sourceDomain), payload, new bytes[](0));

        emit CrosschainDeliveryReclaimed(receiveId, sendId, failed.sourceDomain, failed.sender, failed.amount);
    }

    function _addPeer(uint32 domain, address peerBridge) internal {
        ERC7786TokenBridgeStorage storage $ = _s();
        if (peerBridge == address(0) || domain == block.chainid || $.remoteBridges[domain] != address(0)) {
            revert InvalidPeer(domain);
        }
        $.remoteBridges[domain] = peerBridge;
        emit PeerAdded(domain, peerBridge);
    }

    function _remoteBridge(uint32 domain) internal view returns (bytes memory) {
        address peer = _s().remoteBridges[domain];
        if (peer == address(0)) revert RemoteBridgeNotSet(domain);
        return InteroperableAddress.formatEvmV1(domain, peer);
    }

    function _requireRecipient(address to) internal pure {
        if (to == address(0)) revert InvalidRecipient(to);
    }

    function _encodePayload(address from, address to, uint256 amount, bytes memory extraData)
        internal
        view
        returns (bytes memory)
    {
        return abi.encode(InteroperableAddress.formatEvmV1(block.chainid, from), to, amount, tokenDecimals, extraData);
    }

    function _gasAttributes(uint256 gasLimit) internal pure returns (bytes[] memory attrs) {
        if (gasLimit == 0) return new bytes[](0);
        attrs = new bytes[](1);
        attrs[0] = abi.encodeWithSelector(GAS_LIMIT_ATTR, gasLimit);
    }

    /// @return The amount to credit on the destination. For a lock, this is the amount that arrived,
    ///         so a fee-on-transfer token cannot credit more than the bridge holds.
    function _onSend(address from, uint256 amount) internal returns (uint256) {
        if (mode == TokenBridgeMode.LockUnlock) {
            uint256 before = token.balanceOf(address(this));
            token.safeTransferFrom(from, address(this), amount);
            return token.balanceOf(address(this)) - before;
        } else if (mode == TokenBridgeMode.IssuerBurnMint) {
            IStablecoin(address(token)).burnFrom(from, amount);
        } else {
            IERC7802(address(token)).crosschainBurn(from, amount);
        }
        return amount;
    }

    function _onReceive(address to, uint256 amount) internal {
        if (mode == TokenBridgeMode.LockUnlock) {
            token.safeTransfer(to, amount);
        } else if (mode == TokenBridgeMode.IssuerBurnMint) {
            IStablecoin(address(token)).mint(to, amount);
        } else {
            IERC7802(address(token)).crosschainMint(to, amount);
        }
    }
}
