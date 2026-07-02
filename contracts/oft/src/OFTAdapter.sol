// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {ERC7786TokenBridge} from "./ERC7786TokenBridge.sol";

/// @title OFTAdapter
/// @notice Backwards-compatible lock/unlock adapter name for ERC-7786 token bridges.
contract OFTAdapter is ERC7786TokenBridge {
    constructor(address token_, address bridge_, address owner_)
        ERC7786TokenBridge(token_, bridge_, owner_, ERC7786TokenBridge.TokenBridgeMode.LockUnlock)
    {}
}
