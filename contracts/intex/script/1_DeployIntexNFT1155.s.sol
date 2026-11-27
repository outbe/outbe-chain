// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {INTEX_FACTORY} from "@precompiles/IIntexFactory.sol";
import {GEM_FACTORY} from "@precompiles/IGemFactory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Deploys IntexNFT1155, owned by the deployer. The target router, the NFT router and the
///      auction are its links on every chain; on Outbe the IntexFactory and GemFactory precompiles
///      also burn its units. A proxy that already exists is kept.
///
/// Env: `DEPLOYER_PK` and those of IntexScript.
contract DeployIntexNFT1155 is IntexScript {
    function run() public virtual {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        IntexCreate3.Stack memory s = stack(vm.addr(pk));
        bool outbe = onOutbe();

        vm.startBroadcast(pk);
        deployIntexNFT1155(vm.addr(pk), s, outbe);
        vm.stopBroadcast();

        console.log("IntexNFT1155:", s.intexNFT1155);
    }

    /// @dev Requires an active broadcast from `deployer`.
    function deployIntexNFT1155(address deployer, IntexCreate3.Stack memory s, bool outbe) public {
        if (s.intexNFT1155.code.length != 0) return;
        _deployProxy(
            IntexCreate3.INTEX_NFT1155,
            s.intexNFT1155,
            address(
                new IntexNFT1155(
                    s.intexAuctionTargetRouter,
                    s.intexNFT1155Router,
                    s.intexAuction,
                    outbe ? INTEX_FACTORY : address(0),
                    outbe ? GEM_FACTORY : address(0)
                )
            ),
            abi.encodeCall(IntexNFT1155.initialize, (deployer))
        );
    }
}
