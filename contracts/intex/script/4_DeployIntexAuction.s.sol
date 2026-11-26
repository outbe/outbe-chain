// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Deploys IntexAuction, owned by the deployer, driven by the target router over the escrow and
///      the collection. A proxy that already exists is kept.
///
/// Env: `DEPLOYER_PK` and those of IntexScript.
contract DeployIntexAuction is IntexScript {
    function run() public virtual {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        IntexCreate3.Stack memory s = stack(vm.addr(pk));

        vm.startBroadcast(pk);
        deployIntexAuction(vm.addr(pk), s);
        vm.stopBroadcast();

        console.log("IntexAuction:", s.intexAuction);
    }

    /// @dev Requires an active broadcast from `deployer`.
    function deployIntexAuction(address deployer, IntexCreate3.Stack memory s) public {
        if (s.intexAuction.code.length != 0) return;
        _deployProxy(
            IntexCreate3.INTEX_AUCTION,
            s.intexAuction,
            address(new IntexAuction(s.targetRouter, s.escrowAdapter, s.intexNFT1155)),
            abi.encodeCall(IntexAuction.initialize, (deployer))
        );
    }
}
