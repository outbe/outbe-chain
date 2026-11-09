// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {SALT_VERSION} from "@shared/SaltVersion.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {ERC7786TokenBridge} from "../src/ERC7786TokenBridge.sol";
import {Route, Routes} from "./routes/Routes.sol";

/// @dev Prepares an upgrade of the token bridge of every route on the chains of the stack (see
///      UpgradeProposal). Each implementation gets the token, the hub and the mode of the live
///      proxy; the token decimals follow from the token.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT` (default `SALT_VERSION`), `CREATE3_FACTORY_ADDRESS`,
///   `RPC_URLS`, `SOURCE_COMMIT`.
contract PrepareUpgrade is Routes, UpgradeProposal {
    function _deployerKey() internal view override returns (uint256) {
        return _pk();
    }

    function _proxies(address) internal view override returns (address[] memory proxies) {
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        Route[] memory list = routes();
        proxies = new address[](list.length);
        for (uint256 i = 0; i < list.length; i++) {
            proxies[i] = _bridgeAddress(factory, salt, list[i].spec);
        }
    }

    function _newImplementation(uint256, address proxy) internal override returns (address) {
        ERC7786TokenBridge bridge = ERC7786TokenBridge(proxy);
        return address(new ERC7786TokenBridge(address(bridge.token()), address(bridge.bridge()), bridge.mode()));
    }

    function _deployerOwns(address proxy, address deployer) internal view override returns (bool) {
        return Ownable(proxy).owner() == deployer;
    }
}
