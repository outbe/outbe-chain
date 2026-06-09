// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {OFTAdapter as OFTAdapterBase} from "@layerzerolabs/lz-evm-oapp-v2/contracts/oft/OFTAdapter.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

/// @title OFTAdapter
/// @notice Source-chain OFT adapter that locks/unlocks canonical ERC20 tokens.
contract OFTAdapter is OFTAdapterBase {
    constructor(address token, address lzEndpoint, address delegate)
        OFTAdapterBase(token, lzEndpoint, delegate)
        Ownable(delegate)
    {}
}
