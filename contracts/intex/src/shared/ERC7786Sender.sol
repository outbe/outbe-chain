// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC7786GatewaySource} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";
import {IGatewayQuote} from "@crosschain/interfaces/IGatewayQuote.sol";

/// @title ERC7786Sender
/// @author Outbe
/// @notice Shared base for intex clients that send through the protocol-agnostic ERC-7786 bridge
///         (the `crosschain` hub's `ERC7786Bridge`). The active transport is set on the bridge.
/// @dev Upgrade-safe: the bridge is an implementation immutable, so every upgrade must pass the
///      same bridge to the constructor.
abstract contract ERC7786Sender {
    /// @notice The ERC-7786 bridge this client sends through. Fixed at deploy. To swap the
    ///         cross-chain protocol, change it on the bridge itself (its `setGateway`).
    IERC7786GatewaySource public immutable BRIDGE;

    /// @dev ERC-7786 attribute selector for a per-message destination gas limit. Matches the
    ///      crosschain hub's `GasLimitAttribute.SELECTOR`. Redeclared here (not imported) to keep
    ///      the intex build hub-decoupled.
    bytes4 private constant _GAS_LIMIT_SELECTOR = bytes4(keccak256("executionGasLimit(uint256)"));

    error InvalidBridge();
    error NotEnoughNative(uint256 balance);
    error MsgValueBelowFee(uint256 provided, uint256 required);
    error ReturnFailed();

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address bridge_) {
        if (bridge_ == address(0)) revert InvalidBridge();
        BRIDGE = IERC7786GatewaySource(bridge_);
    }

    /// @dev Sends `payload` to `recipient`, an ERC-7930 address, through the bridge, and funds the
    ///      native fee:
    ///        * relay-funded (`msg.value == 0`): a chain-native module that cannot attach value
    ///          triggered the send. Pay the quoted fee from the contract's native float.
    ///        * entry-funded (`msg.value > 0`): require that the value covers the fee, and refund
    ///          the excess to the caller. Thus an entry caller's buffer never silently seeds (or
    ///          drains) the relay float.
    /// @param gasLimit Destination execution gas for the message (0 = let the active gateway use its default).
    /// @return sendId The bridge's send identifier.
    function _sendTo(bytes memory recipient, bytes memory payload, uint256 gasLimit) internal returns (bytes32) {
        bytes[] memory attributes = _gasAttributes(gasLimit);
        uint256 fee = IGatewayQuote(address(BRIDGE)).quote(recipient, payload, attributes);

        if (msg.value == 0) {
            if (address(this).balance < fee) revert NotEnoughNative(address(this).balance);
        } else {
            _takeFeeFromCaller(fee);
        }

        return BRIDGE.sendMessage{value: fee}(recipient, payload, attributes);
    }

    /// @dev `_sendTo` for a user entry point: the caller always pays the fee, and the native float is never used.
    function _sendToPaidByCaller(bytes memory recipient, bytes memory payload, uint256 gasLimit)
        internal
        returns (bytes32)
    {
        bytes[] memory attributes = _gasAttributes(gasLimit);
        uint256 fee = IGatewayQuote(address(BRIDGE)).quote(recipient, payload, attributes);
        _takeFeeFromCaller(fee);
        return BRIDGE.sendMessage{value: fee}(recipient, payload, attributes);
    }

    /// @dev Native fee to deliver `payload` to `recipient` with `gasLimit` destination gas.
    function _quoteTo(bytes memory recipient, bytes memory payload, uint256 gasLimit) internal view returns (uint256) {
        return IGatewayQuote(address(BRIDGE)).quote(recipient, payload, _gasAttributes(gasLimit));
    }

    /// @dev Require that `msg.value` covers `fee`, and return the excess to the caller.
    function _takeFeeFromCaller(uint256 fee) private {
        if (msg.value < fee) revert MsgValueBelowFee(msg.value, fee);
        uint256 excess = msg.value - fee;
        if (excess > 0) {
            // slither-disable-next-line arbitrary-send-eth
            (bool ok,) = msg.sender.call{value: excess}("");
            if (!ok) revert ReturnFailed();
        }
    }

    /// @dev Wraps `gasLimit` as the single executionGasLimit ERC-7786 attribute (empty array when 0).
    function _gasAttributes(uint256 gasLimit) private pure returns (bytes[] memory attrs) {
        if (gasLimit == 0) return new bytes[](0);
        attrs = new bytes[](1);
        attrs[0] = abi.encodeWithSelector(_GAS_LIMIT_SELECTOR, gasLimit);
    }

    /// @notice Accept native to pre-fund the relay float of the system sends (and receive any bridge fee refunds).
    receive() external payable {}
}
