// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {LayerZeroRouter} from "../src/router/LayerZeroRouter.sol";
import {TypeCasts} from "../src/libs/TypeCasts.sol";

/// @dev Standalone script to configure LayerZero peer addresses and domain mappings.
/// @notice Use this for adding peers after initial deployment. For full setup, prefer 5_ConfigureAll.s.sol.
///
/// Required env vars:
///   DEPLOYER_PK     — deployer private key (must be router owner)
///   ROUTER_ADDRESS  — deployed LayerZeroRouter address
///   PEER_EIDS       — comma-separated LZ endpoint IDs, e.g. "40102,40161"
///   PEER_ADDRESSES  — comma-separated peer contract addresses
///   PEER_DOMAINS    — comma-separated domain IDs matching the EIDs
contract SetupLZPeers is Script {
    function run() public {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address routerAddress = vm.envAddress("ROUTER_ADDRESS");

        uint256[] memory peerEids = vm.envUint("PEER_EIDS", ",");
        address[] memory peerAddresses = vm.envAddress("PEER_ADDRESSES", ",");
        uint256[] memory peerDomains = vm.envUint("PEER_DOMAINS", ",");

        require(peerEids.length == peerAddresses.length, "EIDs and addresses length mismatch");
        require(peerEids.length == peerDomains.length, "EIDs and domains length mismatch");

        vm.startBroadcast(deployerPrivateKey);

        LayerZeroRouter router = LayerZeroRouter(routerAddress);

        for (uint256 i = 0; i < peerEids.length; i++) {
            uint32 eid = uint32(peerEids[i]);
            bytes32 peer = TypeCasts.addressToBytes32(peerAddresses[i]);
            uint32 domain = uint32(peerDomains[i]);

            console2.log("Setting peer for EID:", eid);
            console2.log("  Peer address:", peerAddresses[i]);
            console2.log("  Domain:", domain);

            router.setPeerWithDomain(eid, peer, domain);

            if (i < peerEids.length - 1) {
                vm.sleep(2000);
            }
        }

        vm.stopBroadcast();

        console2.log("Successfully configured", peerEids.length, "peers");
    }
}
