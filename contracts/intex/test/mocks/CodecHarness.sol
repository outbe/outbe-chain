// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {BridgeMsgCodec} from "../../src/shared/libs/BridgeMsgCodec.sol";

/// @dev Test-only wrapper to expose internal BridgeMsgCodec functions.
contract CodecHarness {
    function encodeAuctionResult(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionIntexClearingPrice,
        uint32 wonBidsCount
    ) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeAuctionResult(seriesId, issuedIntexCount, auctionIntexClearingPrice, wonBidsCount);
    }

    function decodeAuctionResult(bytes calldata msg_)
        external
        pure
        returns (uint32 seriesId, uint32 issuedIntexCount, uint64 auctionIntexClearingPrice, uint32 wonBidsCount)
    {
        return BridgeMsgCodec.decodeAuctionResult(msg_);
    }

    function encodeMarkCalled(uint32 seriesId) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeMarkCalled(seriesId);
    }

    function decodeMarkCalled(bytes calldata msg_) external pure returns (uint32 seriesId) {
        return BridgeMsgCodec.decodeMarkCalled(msg_);
    }

    function encodeMarkQualified(uint32 seriesId) external pure returns (bytes memory) {
        return BridgeMsgCodec.encodeMarkQualified(seriesId);
    }

    function decodeMarkQualified(bytes calldata msg_) external pure returns (uint32 seriesId) {
        return BridgeMsgCodec.decodeMarkQualified(msg_);
    }
}
