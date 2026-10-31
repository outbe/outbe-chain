// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

/**
 * @title OrderStatusStorage
 * @notice Shared order status constants, storage and errors for Origin and Destination settlers
 * @dev Prevents diamond inheritance conflicts
 */
abstract contract OrderStatusStorage {
    // ============ Constants ============

    bytes32 public constant UNKNOWN = "";

    // - Origin statuses -
    bytes32 public constant OPENED = "OPENED";
    bytes32 public constant SETTLED = "SETTLED";
    bytes32 public constant REFUNDED = "REFUNDED";

    // - Destination statuses -
    bytes32 public constant CLAIMED = "CLAIMED";
    bytes32 public constant FILLED = "FILLED";

    // ============ Internal Helpers ============

    /// @notice Returns true if the order has not been processed yet (UNKNOWN or OPENED)
    function _isNotProcessed(bytes32 _status) internal pure returns (bool) {
        return _status == UNKNOWN || _status == OPENED;
    }

    // ============ Storage ============

    /// @custom:storage-location erc7201:outbe.intent.OrderStatusStorage
    struct OrderStatusStorageLayout {
        /// @notice Tracks the origin-side lifecycle of each order (UNKNOWN -> OPENED -> SETTLED/REFUNDED)
        mapping(bytes32 orderId => bytes32 status) orderStatus;
        /// @notice Tracks the destination-side lifecycle of each order (UNKNOWN -> CLAIMED -> FILLED).
        ///         It is split from `orderStatus`. Thus a same-chain order (originDomain ==
        ///         destinationDomain) on a single router instance does not collapse both lifecycles into
        ///         one slot.
        mapping(bytes32 orderId => bytes32 status) destinationOrderStatus;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intent.OrderStatusStorage")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _ORDER_STATUS_SLOT = 0xdac90b7480dd88a182663b02afdd59e973c4833e1061e202cbeac05cc2cab400;

    function _statuses() internal pure returns (OrderStatusStorageLayout storage $) {
        assembly ("memory-safe") {
            $.slot := _ORDER_STATUS_SLOT
        }
    }

    function orderStatus(bytes32 orderId) public view returns (bytes32) {
        return _statuses().orderStatus[orderId];
    }

    function destinationOrderStatus(bytes32 orderId) public view returns (bytes32) {
        return _statuses().destinationOrderStatus[orderId];
    }

    // ============ Errors ============

    /// @notice Thrown when the native amount sent doesn't match the required amount
    error InvalidNativeAmount();
}
