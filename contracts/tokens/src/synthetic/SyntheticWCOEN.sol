// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {BridgeableERC20} from "./BridgeableERC20.sol";
import {ERC7786TokenBridge} from "../ERC7786TokenBridge.sol";
import {DEAD_ADDRESS} from "../BurnAddress.sol";

/// @title SyntheticWCOEN
/// @notice WCOEN on a target chain. Its backing is locked on Outbe, so WCOEN sent to 0x…dEaD here
///         burns only when `burnDead` sends it to 0x…dEaD on Outbe.
contract SyntheticWCOEN is BridgeableERC20 {
    uint32 public immutable outbeDomain;

    error InvalidOutbeDomain();
    error NothingToBurn();

    constructor(address tokenBridge_, uint32 outbeDomain_) BridgeableERC20("Wrapped COEN", "WCOEN", 18, tokenBridge_) {
        if (outbeDomain_ == 0) revert InvalidOutbeDomain();
        outbeDomain = outbeDomain_;
    }

    /// @notice Burns the whole balance of 0x…dEaD and sends it to 0x…dEaD on Outbe. Anyone can call
    ///         it; `msg.value` pays the message.
    function burnDead() external payable returns (bytes32 sendId) {
        uint256 amount = balanceOf(DEAD_ADDRESS);
        if (amount == 0) revert NothingToBurn();
        _burn(DEAD_ADDRESS, amount);
        sendId = ERC7786TokenBridge(tokenBridge).sendBurned{value: msg.value}(outbeDomain, amount);
    }
}
