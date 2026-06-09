// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ConfigurableOFT} from "./ConfigurableOFT.sol";

/// @title USDT0OFT
/// @notice USDT0-compatible OFT token with constructor-configured decimals.
contract USDT0OFT is ConfigurableOFT {
    constructor(string memory name_, string memory symbol_, uint8 decimals_, address lzEndpoint, address owner_)
        ConfigurableOFT(name_, symbol_, decimals_, lzEndpoint, owner_)
    {}
}
