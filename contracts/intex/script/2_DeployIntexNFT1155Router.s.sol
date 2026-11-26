// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {RemoteRouter} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Deploys IntexNFT1155Router, owned by the deployer, paired with the router at the same
///      CREATE3 address on every other chain of `PEER_CHAIN_IDS`. A proxy that already exists is kept.
///
/// Env: `DEPLOYER_PK`, `BRIDGE_ADDRESS` (the ERC7786Bridge hub), `PEER_CHAIN_IDS` (comma-separated;
///   this chain is skipped) and those of IntexScript.
contract DeployIntexNFT1155Router is IntexScript {
    function run() public virtual {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        IntexCreate3.Stack memory s = stack(vm.addr(pk));
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        uint256[] memory peers = vm.envUint("PEER_CHAIN_IDS", ",");

        vm.startBroadcast(pk);
        deployIntexNFT1155Router(vm.addr(pk), s, bridge, peers);
        vm.stopBroadcast();

        console.log("IntexNFT1155Router:", s.intexNFT1155Router);
    }

    /// @dev Requires an active broadcast from `deployer`.
    function deployIntexNFT1155Router(
        address deployer,
        IntexCreate3.Stack memory s,
        address bridge,
        uint256[] memory peerChainIds
    ) public {
        if (s.intexNFT1155Router.code.length != 0) return;
        _deployProxy(
            IntexCreate3.INTEX_NFT1155_ROUTER,
            s.intexNFT1155Router,
            address(new IntexNFT1155Router(s.intexNFT1155, bridge)),
            abi.encodeCall(IntexNFT1155Router.initialize, (deployer, remoteRouters(s, peerChainIds)))
        );
    }

    /// @dev The router at the same address on each peer chain other than this one.
    function remoteRouters(IntexCreate3.Stack memory s, uint256[] memory peerChainIds)
        public
        view
        returns (RemoteRouter[] memory routers)
    {
        uint256 n;
        for (uint256 i = 0; i < peerChainIds.length; i++) {
            if (peerChainIds[i] != block.chainid) n++;
        }
        routers = new RemoteRouter[](n);
        n = 0;
        for (uint256 i = 0; i < peerChainIds.length; i++) {
            if (peerChainIds[i] == block.chainid) continue;
            if (peerChainIds[i] > type(uint32).max) revert DomainTooLarge(peerChainIds[i]);
            routers[n++] = RemoteRouter({
                chainId: uint32(peerChainIds[i]),
                interop: InteroperableAddress.formatEvmV1(peerChainIds[i], s.intexNFT1155Router)
            });
        }
    }
}
