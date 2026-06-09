// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {IPromis} from "./interfaces/IPromis.sol";

/// @title MockPromis
/// @notice Mock soulbound Promis ERC20: open minting (no caller-guard), soulbound transfers.
contract MockPromis is ERC20, IPromis {
    constructor() ERC20("Promis", "PROMIS") {}

    /// @inheritdoc IPromis
    function minePromis(address holder, uint256 amount) external {
        if (holder == address(0)) revert ZeroAddress("holder");
        _mint(holder, amount);
        emit Minted(holder, amount);
    }

    function _update(address from, address to, uint256 value) internal override {
        if (from != address(0) && to != address(0)) revert TransfersDisabled();
        super._update(from, to, value);
    }
}
