// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ConfigurableOFT} from "./ConfigurableOFT.sol";

/// @title WCOENOFT
/// @notice WCOEN-compatible OFT token with constructor-configured decimals.
contract WCOENOFT is ConfigurableOFT {
    constructor(string memory name_, string memory symbol_, uint8 decimals_, address lzEndpoint, address owner_)
        ConfigurableOFT(name_, symbol_, decimals_, lzEndpoint, owner_)
    {}
}
