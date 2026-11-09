// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {LoopbackGatewayAdapter} from "src/adapters/LoopbackGatewayAdapter.sol";

/// @dev Prepares an upgrade of the hub and of every adapter deployed on the chains of the stack
///      (see UpgradeProposal). Each implementation gets the immutables of the live proxy: the
///      mailbox, the LayerZero endpoint or the hub.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT` (default `SALT_VERSION`), `CREATE3_FACTORY_ADDRESS`,
///   `RPC_URLS`, `SOURCE_COMMIT`.
contract PrepareUpgrade is UpgradeProposal {
    function _deployerKey() internal view override returns (uint256) {
        return vm.envUint("DEPLOYER_PK");
    }

    function _proxies(address deployer) internal view override returns (address[] memory proxies) {
        string[4] memory labels =
            ["ERC7786Bridge", "HyperlaneGatewayAdapter", "LayerZeroGatewayAdapter", "LoopbackGatewayAdapter"];
        Create3Factory factory = Create3Factory(vm.envAddress("CREATE3_FACTORY_ADDRESS"));
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        proxies = new address[](labels.length);
        for (uint256 i = 0; i < labels.length; i++) {
            proxies[i] = factory.predict(deployer, keccak256(abi.encodePacked(labels[i], salt)));
        }
    }

    function _newImplementation(uint256 index, address proxy) internal override returns (address) {
        if (index == 0) return address(new ERC7786Bridge());
        if (index == 1) return address(new HyperlaneGatewayAdapter(address(HyperlaneGatewayAdapter(proxy).MAILBOX())));
        if (index == 2) {
            return address(new LayerZeroGatewayAdapter(address(LayerZeroGatewayAdapter(proxy).endpoint())));
        }
        return address(new LoopbackGatewayAdapter(LoopbackGatewayAdapter(proxy).HUB()));
    }

    function _deployerOwns(address proxy, address deployer) internal view override returns (bool) {
        return Ownable(proxy).owner() == deployer;
    }
}
