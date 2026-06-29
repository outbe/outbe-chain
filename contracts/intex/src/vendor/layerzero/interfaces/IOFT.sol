// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {
    MessagingFee,
    MessagingReceipt
} from "@layerzerolabs/lz-evm-protocol-v2/contracts/interfaces/ILayerZeroEndpointV2.sol";

struct SendParam {
    uint32 dstEid;
    bytes32 to;
    uint256 amountLD;
    uint256 minAmountLD;
    bytes extraOptions;
    bytes composeMsg;
    bytes oftCmd;
}

struct OFTReceipt {
    uint256 amountSentLD;
    uint256 amountReceivedLD;
}

/// @notice Minimal subset of the LayerZero OFT v2 interface used to route auction proceeds.
interface IOFT {
    function send(SendParam calldata sendParam, MessagingFee calldata fee, address refundAddress)
        external
        payable
        returns (MessagingReceipt memory, OFTReceipt memory);

    function quoteSend(SendParam calldata sendParam, bool payInLzToken) external view returns (MessagingFee memory);
}
