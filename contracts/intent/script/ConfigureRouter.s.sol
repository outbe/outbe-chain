// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {CreateX} from "./0_DeployCreateX.s.sol";
import {Router} from "../src/router/Router.sol";

/// @dev Registers the matching Router on each remote chain. Routers share one CREATE3 address across chains, so the
///      remote address equals the local (computed here) — env only lists chain ids.
///
/// Required env vars (DEPLOYER_PK must be the Router owner):
///   DEPLOYER_PK      — owner private key
///   CONTRACT_SALT    — salt string used at deploy
///   CREATEX_ADDRESS  — deployed CreateX factory
///   ROUTER_ADDRESS   — local Router to configure
///   REMOTE_CHAIN_IDS — csv of remote EVM chain ids
contract ConfigureRouter is Script {
    function run() public {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address createX = vm.envAddress("CREATEX_ADDRESS");
        address routerAddress = vm.envAddress("ROUTER_ADDRESS");
        address deployer = vm.addr(deployerPk);

        uint256[] memory remoteChainIds = vm.envUint("REMOTE_CHAIN_IDS", ",");

        address remoteRouterAddr =
            CreateX(createX).computeCreate3Address(keccak256(abi.encodePacked("Router", salt, deployer)));

        vm.startBroadcast(deployerPk);

        Router router = Router(routerAddress);
        for (uint256 i = 0; i < remoteChainIds.length; i++) {
            uint256 chainId = remoteChainIds[i];
            router.setRemoteRouter(uint32(chainId), InteroperableAddress.formatEvmV1(chainId, remoteRouterAddr));
            console2.log("remote Router set for chainId:", chainId);
        }

        vm.stopBroadcast();
    }
}
