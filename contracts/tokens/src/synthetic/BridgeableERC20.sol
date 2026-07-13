// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ConfigurableERC7802} from "../ConfigurableERC7802.sol";

/// @title BridgeableERC20
/// @notice ERC-7802 bridgeable ERC20 with constructor-configured metadata and decimals.
contract BridgeableERC20 is ConfigurableERC7802 {
    constructor(string memory name_, string memory symbol_, uint8 decimals_, address owner_)
        ConfigurableERC7802(name_, symbol_, decimals_, owner_)
    {}
}
