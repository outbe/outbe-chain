// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {ERC20Bridgeable} from "@openzeppelin/contracts/token/ERC20/extensions/draft-ERC20Bridgeable.sol";

/// @title BridgeableERC20
/// @notice ERC-7802 bridgeable ERC20 with constructor-configured metadata and decimals.
/// @dev The token has no owner. The constructor fixes the only address that can mint and burn, so
///      nobody can point the mint authority at another contract later.
contract BridgeableERC20 is ERC20Bridgeable {
    uint8 private immutable _LOCAL_DECIMALS;

    /// @notice The only address that can call `crosschainMint` and `crosschainBurn`.
    address public immutable tokenBridge;

    error InvalidTokenBridge();
    error UnauthorizedTokenBridge(address caller);

    /// @param tokenBridge_ The bridge address. The deploy script predicts it with CREATE3, because
    ///        the bridge is deployed after the token.
    constructor(string memory name_, string memory symbol_, uint8 decimals_, address tokenBridge_)
        ERC20(name_, symbol_)
    {
        if (tokenBridge_ == address(0)) revert InvalidTokenBridge();
        _LOCAL_DECIMALS = decimals_;
        tokenBridge = tokenBridge_;
    }

    function decimals() public view override returns (uint8) {
        return _LOCAL_DECIMALS;
    }

    function _checkTokenBridge(address caller) internal view override {
        if (caller != tokenBridge) revert UnauthorizedTokenBridge(caller);
    }
}
