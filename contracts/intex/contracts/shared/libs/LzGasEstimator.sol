// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {OptionsBuilder} from "@layerzerolabs/oapp-evm/oapp/libs/OptionsBuilder.sol";

/// @title LzGasEstimator
/// @notice Builds LayerZero executor `lzReceiveOption` blobs whose destination gas limit scales
///         with the number of items the inbound handler will iterate, plus a safety buffer.
/// @dev A fixed gas option (e.g. `lzReceiveOption(200000, 0)`) sized for a single item runs the
///      destination `_lzReceive` out of gas for a large batch — the whole packet reverts and the
///      LZ channel stalls. Sizing the option on the outbound side from `(baseGas + perItemGas *
///      itemCount) * buffer` keeps destination liveness independent of payload size.
/// @author Outbe
library LzGasEstimator {
    using OptionsBuilder for bytes;

    /// @notice Default safety buffer applied on top of the raw estimate, in basis points.
    /// @dev 2000 bps = +20%. Absorbs estimation drift (storage-cold vs warm, recipient hooks).
    uint16 internal constant DEFAULT_BUFFER_BPS = 2000;

    /// @notice Raw (un-buffered) destination gas estimate for `itemCount` items.
    /// @param baseGas Fixed overhead: decode + dispatch + item-count-independent work.
    /// @param perItemGas Marginal gas per loop iteration on the destination handler.
    /// @param itemCount Number of items the destination will iterate.
    /// @return The raw destination gas estimate, before any safety buffer is applied.
    function estimateGas(uint128 baseGas, uint128 perItemGas, uint256 itemCount) internal pure returns (uint256) {
        return uint256(baseGas) + uint256(perItemGas) * itemCount;
    }

    /// @notice Build an `lzReceiveOption` for `itemCount` items with the default buffer.
    /// @param baseGas Fixed overhead: decode + dispatch + item-count-independent work.
    /// @param perItemGas Marginal gas per loop iteration on the destination handler.
    /// @param itemCount Number of items the destination will iterate.
    /// @return The encoded `lzReceiveOption` blob with the default safety buffer applied.
    function receiveOption(
        uint128 baseGas,
        uint128 perItemGas,
        uint256 itemCount
    ) internal pure returns (bytes memory) {
        return receiveOption(baseGas, perItemGas, itemCount, DEFAULT_BUFFER_BPS);
    }

    /// @notice Build an `lzReceiveOption` for `itemCount` items with a custom buffer.
    /// @param baseGas Fixed overhead: decode + dispatch + item-count-independent work.
    /// @param perItemGas Marginal gas per loop iteration on the destination handler.
    /// @param itemCount Number of items the destination will iterate.
    /// @param bufferBps Safety buffer in basis points (e.g. 2000 = +20%).
    /// @return The encoded `lzReceiveOption` blob with `bufferBps` applied on top of the estimate.
    function receiveOption(
        uint128 baseGas,
        uint128 perItemGas,
        uint256 itemCount,
        uint16 bufferBps
    ) internal pure returns (bytes memory) {
        uint256 raw = estimateGas(baseGas, perItemGas, itemCount);
        uint256 buffered = (raw * (10_000 + uint256(bufferBps))) / 10_000;
        // `addExecutorLzReceiveOption` takes a uint128 gas; a payload large enough to overflow it
        // is far beyond any realistic batch and would itself exceed block gas limits.
        return OptionsBuilder.newOptions().addExecutorLzReceiveOption(uint128(buffered), 0);
    }
}
