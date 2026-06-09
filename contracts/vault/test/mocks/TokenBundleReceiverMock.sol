// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity ^0.8.0;

import {ITokenBundle} from "../../src/interfaces/ITokenBundle.sol";
import {IERC20} from "../../src/interfaces/IERC20.sol";

/// @notice Mock receiver that implements ITokenBundle.topUp, simulating a Credis smart account.
contract TokenBundleReceiverMock is ITokenBundle {
    event TopUpCalled(address sender, address token, uint256 amount);

    function topUp(address sender, address token, uint256 amount) external override {
        IERC20(token).transferFrom(sender, address(this), amount);
        emit TopUpCalled(sender, token, amount);
    }

    function balanceOf(address, address token) external view override returns (uint256) {
        return IERC20(token).balanceOf(address(this));
    }
}
