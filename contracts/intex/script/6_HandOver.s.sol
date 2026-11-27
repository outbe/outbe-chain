// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Gives the five Intex contracts on this chain to governance: CrosschainBridge on Outbe and its
///      Interchain Account on any other chain. Each is a plain Ownable, so `transferOwnership` moves
///      the owner at once. The script reads every owner back. Run it last, after DeployAll.
///      After it, an upgrade or a change of a remote router or of the escrow asset is an
///      `upgradeContracts` vote of Update.
///
/// Env: `DEPLOYER_PK` (the current owner) and those of IntexScript.
contract HandOver is IntexScript {
    error NotHandedOver(address proxy, address owner);

    function run() public virtual {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        IntexCreate3.Stack memory s = stack(vm.addr(pk));
        // Read before the broadcast: off Outbe this asks the Outbe node.
        address governance = governanceOwner();

        vm.startBroadcast(pk);
        handOver(governance, s);
        vm.stopBroadcast();

        console.log("handed over to governance:", governance);
    }

    /// @dev Requires an active broadcast from the current owner. A contract governance already owns
    ///      is skipped.
    function handOver(address governance, IntexCreate3.Stack memory s) public {
        address[5] memory owned = IntexCreate3.proxies(s);
        for (uint256 i = 0; i < owned.length; i++) {
            Ownable c = Ownable(owned[i]);
            if (c.owner() != governance) c.transferOwnership(governance);
            if (c.owner() != governance) revert NotHandedOver(owned[i], c.owner());
        }
    }
}
