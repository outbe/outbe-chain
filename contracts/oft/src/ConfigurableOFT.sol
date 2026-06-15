// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {OFTCore} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oft/OFTCore.sol";

/// @title ConfigurableOFT
/// @notice LayerZero OFT with constructor-configured ERC20 metadata and local decimals.
abstract contract ConfigurableOFT is OFTCore, ERC20 {
    uint8 private immutable _LOCAL_DECIMALS;

    constructor(string memory name_, string memory symbol_, uint8 decimals_, address lzEndpoint, address owner_)
        ERC20(name_, symbol_)
        OFTCore(decimals_, lzEndpoint, owner_)
        Ownable(owner_)
    {
        _LOCAL_DECIMALS = decimals_;
    }

    function decimals() public view override returns (uint8) {
        return _LOCAL_DECIMALS;
    }

    function token() public view returns (address) {
        return address(this);
    }

    function approvalRequired() external pure virtual returns (bool) {
        return false;
    }

    function _debit(address from, uint256 amountLD, uint256 minAmountLD, uint32 dstEid)
        internal
        virtual
        override
        returns (uint256 amountSentLD, uint256 amountReceivedLD)
    {
        (amountSentLD, amountReceivedLD) = _debitView(amountLD, minAmountLD, dstEid);
        _burn(from, amountSentLD);
    }

    function _credit(address to, uint256 amountLD, uint32)
        internal
        virtual
        override
        returns (uint256 amountReceivedLD)
    {
        if (to == address(0)) to = address(0xdead);
        _mint(to, amountLD);
        return amountLD;
    }
}
