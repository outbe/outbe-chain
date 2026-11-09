// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {ERC7786TokenBridge} from "../src/ERC7786TokenBridge.sol";
import {Route, Routes} from "./routes/Routes.sol";

/// @dev Prepares an upgrade of the token bridge of every route on the chains of the stack (see
///      UpgradeProposal). Each implementation gets the token, the hub and the mode of the live
///      proxy; the token decimals follow from the token.
///
/// Env: the UpgradeProposal ones, `CREATE3_FACTORY_ADDRESS` and `CONTRACT_SALT` (default `SALT_VERSION`).
contract PrepareUpgrade is Routes, UpgradeProposal {
    function _proxies(address deployer) internal view override returns (address[] memory proxies) {
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        Route[] memory list = routes();
        proxies = new address[](list.length);
        for (uint256 i = 0; i < list.length; i++) {
            proxies[i] = Create3Factory(factory)
                .predict(deployer, _salt(string.concat(list[i].spec.tokenLabel, "Bridge"), salt));
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
