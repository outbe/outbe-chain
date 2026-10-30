// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {Create3Factory} from "./Create3Factory.sol";

/// @title Create3Proxy
/// @notice Deploys a UUPS ERC1967 proxy through the CREATE3 factory. The proxy address depends only on
///         the factory, the caller and the salt, so it does not change with the implementation.
library Create3Proxy {
    function deploy(Create3Factory factory, bytes32 salt, address implementation, bytes memory initData)
        internal
        returns (address)
    {
        return factory.deploy(
            salt, abi.encodePacked(type(ERC1967Proxy).creationCode, abi.encode(implementation, initData))
        );
    }
}
