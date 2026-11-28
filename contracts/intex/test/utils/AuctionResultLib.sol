// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev The result of a day for one chain, with no won quantity and no demand unless a case needs them.
library AuctionResultLib {
    function day(uint32 quantity, uint64 clearingRate, uint32 wonBidsCount)
        internal
        pure
        returns (BridgeMsgCodec.IntexAuctionChainResult memory)
    {
        return BridgeMsgCodec.IntexAuctionChainResult({
            wonQuantity: 0, wonBidsCount: wonBidsCount, quantity: quantity, clearingRate: clearingRate, totalDemand: 0
        });
    }
}
