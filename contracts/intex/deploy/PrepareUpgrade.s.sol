// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";

/// @title PrepareUpgrade
/// @author Outbe
/// @notice Prepares an upgrade of every intex proxy on every chain of the stack (see
///         UpgradeProposal). Each implementation gets the immutables of the live proxy: the
///         bridge hub, the bridged NFT, the Outbe chain id and the ERC20Router of WCOEN.
/// @dev Env: the UpgradeProposal ones, CREATE3_FACTORY_ADDRESS and CONTRACT_SALT (optional). The
///      upgrade authority is DEFAULT_ADMIN_ROLE.
contract PrepareUpgrade is BaseScript, UpgradeProposal {
    function _proxies(address deployer) internal view override returns (address[] memory proxies) {
        Create3Factory factory = create3Factory();
        string[5] memory prefixes = intexProxyPrefixes();
        proxies = new address[](prefixes.length);
        for (uint256 i = 0; i < prefixes.length; i++) {
            proxies[i] = predictProxy(factory, deployer, prefixes[i]);
        }
    }

    function _newImplementation(uint256 index, address proxy) internal override returns (address) {
        if (index == 0) return address(new IntexNFT1155());
        if (index == 1) return address(new EscrowAdapter());
        if (index == 2) return address(new IntexAuction());
        if (index == 3) {
            IntexNFT1155Router bridge = IntexNFT1155Router(payable(proxy));
            return address(new IntexNFT1155Router(address(bridge.token()), address(bridge.BRIDGE())));
        }
        TargetRouter router = TargetRouter(payable(proxy));
        return
            address(new TargetRouter(address(router.BRIDGE()), router.OUTBE_CHAIN_ID(), address(router.erc20Router())));
    }

    function _deployerOwns(address proxy, address deployer) internal view override returns (bool) {
        return IAccessControl(proxy).hasRole(bytes32(0), deployer);
    }
}
