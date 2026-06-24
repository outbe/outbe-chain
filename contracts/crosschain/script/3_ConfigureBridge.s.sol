// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {CreateX} from "./0_DeployCreateX.s.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";

/// @dev Wires the local hub to its counterparts on other chains. Bridge and adapters share one CREATE3 address across
///      chains, so remote addresses equal the local ones (computed here) — env only lists `(chainId, eid)`.
///      Each adapter is wired only if its endpoint env is present (LZ_ENDPOINT / HYPERLANE_MAILBOX). For Hyperlane the
///      remote domain is assumed equal to the chain id.
///
/// Required env (DEPLOYER_PK must be BRIDGE_OWNER): `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATEX_ADDRESS`,
/// `REMOTE_CHAIN_IDS` (csv); `REMOTE_EIDS` (csv, parallel) when wiring LayerZero.
contract ConfigureBridge is Script {
    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address createX = vm.envAddress("CREATEX_ADDRESS");
        address deployer = vm.addr(deployerPk);

        uint256[] memory remoteChainIds = vm.envUint("REMOTE_CHAIN_IDS", ",");

        bool hasLz = vm.envOr("LZ_ENDPOINT", address(0)) != address(0);
        bool hasHl = vm.envOr("HYPERLANE_MAILBOX", address(0)) != address(0);

        address bridgeAddr = _compute(createX, salt, deployer, "ERC7786Bridge");
        address lzAdapter = hasLz ? _compute(createX, salt, deployer, "LayerZeroGatewayAdapter") : address(0);
        address hlAdapter = hasHl ? _compute(createX, salt, deployer, "HyperlaneGatewayAdapter") : address(0);

        uint256[] memory remoteEids;
        if (hasLz) {
            remoteEids = vm.envUint("REMOTE_EIDS", ",");
            require(remoteEids.length == remoteChainIds.length, "REMOTE_EIDS/REMOTE_CHAIN_IDS length mismatch");
        }

        vm.startBroadcast(deployerPk);

        for (uint256 i = 0; i < remoteChainIds.length; i++) {
            uint256 chainId = remoteChainIds[i];

            // Remote bridge shares the local (CREATE3) address.
            ERC7786Bridge(bridgeAddr).registerRemoteBridge(InteroperableAddress.formatEvmV1(chainId, bridgeAddr));

            if (hasLz) {
                LayerZeroGatewayAdapter(lzAdapter)
                    .setPeerWithChain(uint32(remoteEids[i]), bytes32(uint256(uint160(lzAdapter))), chainId);
            }
            if (hasHl) {
                HyperlaneGatewayAdapter(hlAdapter)
                    .setRouterWithChain(uint32(chainId), bytes32(uint256(uint160(hlAdapter))), chainId);
            }

            console2.log("wired remote chainId:", chainId);
        }

        vm.stopBroadcast();
    }

    function _compute(address createX, string memory salt, address deployer, string memory label)
        internal
        view
        returns (address)
    {
        return CreateX(createX).computeCreate3Address(keccak256(abi.encodePacked(label, salt, deployer)));
    }
}
