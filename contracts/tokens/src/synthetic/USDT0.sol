// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ConfigurableERC7802} from "../ConfigurableERC7802.sol";
import {IReferenceCurrency} from "../interfaces/IReferenceCurrency.sol";

/// @title USDT0
/// @notice USDT0-compatible ERC-7802 bridgeable token with constructor-configured decimals.
contract USDT0 is ConfigurableERC7802, IReferenceCurrency {
    constructor(string memory name_, string memory symbol_, uint8 decimals_, address owner_)
        ConfigurableERC7802(name_, symbol_, decimals_, owner_)
    {}

    /// @inheritdoc IReferenceCurrency
    /// @dev USDT0 denominates US dollars, so the ISO 4217 numeric code is 840 (USD).
    function isoCode() external pure returns (uint16) {
        return 840;
    }
}
