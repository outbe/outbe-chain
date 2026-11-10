// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";
import {Scope} from "the-compact/src/types/Scope.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";
import {SolverAllocator} from "../src/allocators/SolverAllocator.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";
import {IntentCreate3} from "./IntentCreate3.sol";

/// @dev Deployment script for solver collateral system. The allocator and the escrow are UUPS
///      proxies at CREATE3 addresses, owned by the deployer. A proxy that already exists is kept.
///
/// Required env vars:
///   DEPLOYER_PK           - deployer private key
///   CREATE3_FACTORY_ADDRESS - deployed Create3Factory (from contracts/shared)
///   CONTRACT_SALT         - salt string for deterministic deployment (default SALT_VERSION)
///   COMPACT_ADDRESS       - The Compact address
///   COLLATERAL_BPS        - collateral requirement in basis points (e.g. 1000 = 10%)
contract DeploySolverEscrow is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        address compact = vm.envAddress("COMPACT_ADDRESS");
        uint256 collateralBps = vm.envOr("COLLATERAL_BPS", uint256(1000));

        vm.startBroadcast(deployerPrivateKey);
        address escrow = deployEscrow(factory, salt, compact, collateralBps, vm.addr(deployerPrivateKey));
        vm.stopBroadcast();

        console2.log("SolverEscrow deployed at:", escrow);
        console2.log("NOTE: Call escrow.setAuthorizedCaller(router) after deploying the router.");
    }

    /// @dev `deployer` sends the factory calls and owns both proxies.
    function deployEscrow(address factory, string memory salt, address compact, uint256 collateralBps, address deployer)
        public
        returns (address escrow)
    {
        address allocator = IntentCreate3.predict(factory, deployer, IntentCreate3.SOLVER_ALLOCATOR, salt);
        if (allocator.code.length == 0) {
            Create3Proxy.deploy(
                Create3Factory(factory),
                IntentCreate3.salt(IntentCreate3.SOLVER_ALLOCATOR, salt),
                address(new SolverAllocator(compact)),
                abi.encodeCall(SolverAllocator.initialize, (deployer))
            );
        }
        bytes12 lockTag = SolverAllocator(allocator).buildLockTag(Scope.ChainSpecific, ResetPeriod.ThirtyDays);

        escrow = IntentCreate3.predict(factory, deployer, IntentCreate3.SOLVER_ESCROW, salt);
        if (escrow.code.length == 0) {
            Create3Proxy.deploy(
                Create3Factory(factory),
                IntentCreate3.salt(IntentCreate3.SOLVER_ESCROW, salt),
                address(new SolverEscrow(compact, lockTag)),
                abi.encodeCall(SolverEscrow.initialize, (deployer, collateralBps))
            );
        }
        if (SolverAllocator(allocator).arbiter() == address(0)) SolverAllocator(allocator).setArbiter(escrow);
        console2.log("SolverAllocator:", allocator);
    }
}
