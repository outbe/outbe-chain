// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";

/// @dev An issuance message carries the set of series a chain receives from one day. Tests that
///      exercise a single series wrap it in a one-element batch.
function _asBatch(BridgeMsgCodec.IssuanceInstructionsPayload memory one)
    pure
    returns (BridgeMsgCodec.IssuanceInstructionsPayload[] memory batch)
{
    batch = new BridgeMsgCodec.IssuanceInstructionsPayload[](1);
    batch[0] = one;
}
