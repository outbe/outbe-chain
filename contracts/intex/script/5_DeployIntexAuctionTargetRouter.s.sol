// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Deploys IntexAuctionTargetRouter, owned by the deployer. It takes the messages of Outbe only
///      from the Interchain Account of Outbe on this chain, so Hyperlane must know this chain before
///      this runs. A proxy that already exists is kept.
///
/// Env: `DEPLOYER_PK`, `BRIDGE_ADDRESS` (the ERC7786Bridge hub), `WCOEN_BRIDGE` (the ERC20Router of
///   WCOEN, which carries the proceeds) and those of IntexScript.
contract DeployIntexAuctionTargetRouter is IntexScript {
    /// @dev What the router gets besides the other Intex contracts.
    struct Transport {
        address bridge;
        uint32 outbeChainId;
        address wcoenBridge;
        address outbeIca;
    }

    function run() public virtual {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        IntexCreate3.Stack memory s = stack(vm.addr(pk));
        Transport memory t = transport();

        vm.startBroadcast(pk);
        deployIntexAuctionTargetRouter(vm.addr(pk), s, t);
        vm.stopBroadcast();

        console.log("IntexAuctionTargetRouter:", s.intexAuctionTargetRouter);
        console.log("  Interchain Account of Outbe:", t.outbeIca);
    }

    /// @dev Read before a broadcast: off Outbe the account comes from the Outbe node.
    function transport() public returns (Transport memory) {
        return Transport({
            bridge: vm.envAddress("BRIDGE_ADDRESS"),
            outbeChainId: outbeChainId(),
            wcoenBridge: vm.envAddress("WCOEN_BRIDGE"),
            outbeIca: outbeAccount()
        });
    }

    /// @dev Requires an active broadcast from `deployer`.
    function deployIntexAuctionTargetRouter(address deployer, IntexCreate3.Stack memory s, Transport memory t) public {
        if (s.intexAuctionTargetRouter.code.length != 0) return;
        _deployProxy(
            IntexCreate3.INTEX_AUCTION_TARGET_ROUTER,
            s.intexAuctionTargetRouter,
            address(
                new IntexAuctionTargetRouter(
                    t.bridge, t.outbeChainId, t.wcoenBridge, s.intexAuction, s.intexNFT1155, s.escrowAdapter, t.outbeIca
                )
            ),
            abi.encodeCall(IntexAuctionTargetRouter.initialize, (deployer))
        );
    }
}
