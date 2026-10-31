// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";
import {Scope} from "the-compact/src/types/Scope.sol";

import {SolverAllocator} from "../src/allocators/SolverAllocator.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";

/// @dev Deployment script for solver collateral system. The allocator and the escrow are UUPS
///      proxies owned by the deployer.
///
/// Required env vars:
///   DEPLOYER_PK           - deployer private key
///   COMPACT_ADDRESS       - The Compact address
///   COLLATERAL_BPS        - collateral requirement in basis points (e.g. 1000 = 10%)
contract DeploySolverEscrow is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address compact = vm.envAddress("COMPACT_ADDRESS");
        uint256 collateralBps = vm.envOr("COLLATERAL_BPS", uint256(1000));

        vm.startBroadcast(deployerPrivateKey);
        address escrow = deployEscrow(compact, collateralBps, vm.addr(deployerPrivateKey));
        vm.stopBroadcast();

        console2.log("SolverEscrow deployed at:", escrow);
        console2.log("NOTE: Call escrow.setAuthorizedCaller(router) after deploying the router.");
    }

    function deployEscrow(address compact, uint256 collateralBps, address owner) public returns (address) {
        SolverAllocator allocator = SolverAllocator(
            address(
                new ERC1967Proxy(
                    address(new SolverAllocator(compact)), abi.encodeCall(SolverAllocator.initialize, (owner))
                )
            )
        );
        bytes12 lockTag = allocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.ThirtyDays);
        SolverEscrow escrow = SolverEscrow(
            address(
                new ERC1967Proxy(
                    address(new SolverEscrow(compact, lockTag)),
                    abi.encodeCall(SolverEscrow.initialize, (owner, collateralBps))
                )
            )
        );
        allocator.setArbiter(address(escrow));
        return address(escrow);
    }
}
