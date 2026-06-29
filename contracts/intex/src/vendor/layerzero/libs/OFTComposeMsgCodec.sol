// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @notice Minimal decoder for a LayerZero OFT compose message.
/// @dev Layout: nonce(8) | srcEid(4) | amountLD(32) | composeFrom(32) | composeMsg(...).
library OFTComposeMsgCodec {
    uint256 private constant AMOUNT_LD_OFFSET = 12;
    uint256 private constant COMPOSE_FROM_OFFSET = 44;
    uint256 private constant COMPOSE_MSG_OFFSET = 76;

    /// @notice Amount delivered in local decimals.
    function amountLD(bytes calldata _msg) internal pure returns (uint256) {
        return uint256(bytes32(_msg[AMOUNT_LD_OFFSET:COMPOSE_FROM_OFFSET]));
    }

    /// @notice The caller-supplied compose payload.
    function composeMsg(bytes calldata _msg) internal pure returns (bytes memory) {
        return _msg[COMPOSE_MSG_OFFSET:];
    }
}
