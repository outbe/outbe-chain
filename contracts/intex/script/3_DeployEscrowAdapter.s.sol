// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Deploys EscrowAdapter, owned by the deployer, for the auction and the target router, with
///      WCOEN in The Compact as its asset. A proxy that already exists is kept.
///
/// Env: `DEPLOYER_PK`, `COMPACT_ADDRESS`, `WCOEN_TOKEN` and those of IntexScript.
contract DeployEscrowAdapter is IntexScript {
    function run() public virtual {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        IntexCreate3.Stack memory s = stack(vm.addr(pk));
        address compact = vm.envAddress("COMPACT_ADDRESS");
        address wcoen = vm.envAddress("WCOEN_TOKEN");

        vm.startBroadcast(pk);
        deployEscrowAdapter(vm.addr(pk), s, compact, wcoen);
        vm.stopBroadcast();

        console.log("EscrowAdapter:", s.escrowAdapter);
    }

    /// @dev Requires an active broadcast from `deployer`.
    function deployEscrowAdapter(address deployer, IntexCreate3.Stack memory s, address compact, address asset) public {
        if (s.escrowAdapter.code.length != 0) return;
        _deployProxy(
            IntexCreate3.ESCROW_ADAPTER,
            s.escrowAdapter,
            address(new EscrowAdapter(s.intexAuction, s.targetRouter)),
            abi.encodeCall(EscrowAdapter.initialize, (deployer, compact, asset))
        );
    }
}
