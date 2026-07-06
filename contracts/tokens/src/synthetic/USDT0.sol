// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ConfigurableERC7802} from "../ConfigurableERC7802.sol";

/// @title USDT0
/// @notice USDT0-compatible ERC-7802 bridgeable token with constructor-configured decimals.
contract USDT0 is ConfigurableERC7802 {
    constructor(string memory name_, string memory symbol_, uint8 decimals_, address owner_)
        ConfigurableERC7802(name_, symbol_, decimals_, owner_)
    {}
}
