// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {IERC7786GatewaySource} from "../interfaces/IERC7786.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {GAS_LIMIT_ATTR, REFUND_ADDRESS_ATTR} from "@shared/GatewayAttributes.sol";

/**
 * @dev The ERC-7786 attributes the gateway adapters understand: a per-message destination
 * execution gas limit, and the address that gets the extra native fee back.
 *
 * Applications pass it in the `attributes` array of {IERC7786GatewaySource-sendMessage} (and the
 * matching `quote`). Each adapter translates the transport-agnostic value into its native mechanism
 * (LayerZero executor options, Hyperlane hook metadata). When the attribute is absent, the adapter
 * uses its own `defaultGasLimit`.
 */
library GasLimitAttribute {
    /// @dev `bytes4(keccak256("executionGasLimit(uint256)"))`.
    bytes4 internal constant SELECTOR = GAS_LIMIT_ATTR;

    /// @dev `bytes4(keccak256("refundAddress(address)"))`. The transport sends the part of
    /// `msg.value` above its fee to this address. Without the attribute, the address is the caller
    /// of the adapter (the hub), which cannot receive native value, so the send needs the exact fee.
    bytes4 internal constant REFUND_SELECTOR = REFUND_ADDRESS_ATTR;

    /// @dev ABI-encodes the refundAddress attribute, for use in an ERC-7786 `attributes` array.
    function encodeRefund(address to) internal pure returns (bytes memory) {
        return abi.encodeWithSelector(REFUND_SELECTOR, to);
    }

    /// @dev Returns the refundAddress attribute (the last occurrence wins), or `fallbackAddress`
    /// when the attribute is absent or zero.
    function refundAddress(bytes[] calldata attributes, address fallbackAddress) internal pure returns (address to) {
        to = fallbackAddress;
        for (uint256 i = 0; i < attributes.length; i++) {
            bytes calldata attribute = attributes[i];
            if (attribute.length >= 4 && bytes4(attribute[0:4]) == REFUND_SELECTOR) {
                address decoded = abi.decode(attribute[4:], (address));
                if (decoded != address(0)) to = decoded;
            }
        }
    }

    /// @dev ABI-encodes the attribute carrying `gasLimit`, for use in an ERC-7786 `attributes`
    /// array.
    function encode(uint256 gasLimit) internal pure returns (bytes memory) {
        return abi.encodeWithSelector(SELECTOR, gasLimit);
    }

    /**
     * @dev Scans `attributes` for the executionGasLimit attribute. Returns whether the attribute is
     * present and the decoded gas limit (the last occurrence wins). Skips the refundAddress
     * attribute. Reverts {IERC7786GatewaySource-UnsupportedAttribute} for any other attribute, as
     * ERC-7786 requires.
     */
    function find(bytes[] calldata attributes) internal pure returns (bool found, uint256 gasLimit) {
        for (uint256 i = 0; i < attributes.length; i++) {
            bytes calldata attribute = attributes[i];
            bytes4 selector = attribute.length < 4 ? bytes4(0) : bytes4(attribute[0:4]);
            if (selector == REFUND_SELECTOR) continue;
            if (selector != SELECTOR) revert IERC7786GatewaySource.UnsupportedAttribute(selector);
            gasLimit = abi.decode(attribute[4:], (uint256));
            found = true;
        }
    }

    /**
     * @dev Resolves the destination gas for a message. The result is the executionGasLimit
     * attribute bounded to `uint128`, or `defaultGasLimit` when the attribute is absent. Reverts as
     * {find} for any other attribute, or via {SafeCast} if the requested gas exceeds `uint128`.
     */
    function resolve(bytes[] calldata attributes, uint128 defaultGasLimit) internal pure returns (uint128) {
        (bool found, uint256 gasLimit) = find(attributes);
        return found ? SafeCast.toUint128(gasLimit) : defaultGasLimit;
    }
}
