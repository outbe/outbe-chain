// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {console2} from "forge-std/console2.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {DeploySolverEscrow} from "./1_DeploySolverEscrow.s.sol";
import {DeployAuction} from "./2_DeployAuction.s.sol";
import {DeployRouter} from "./3_DeployRouter.s.sol";
import {ConfigureAll} from "./4_ConfigureAll.s.sol";
import {HandOver, IntentContracts} from "./5_HandOver.s.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";

/// @dev The contracts outside this project that the intent stack talks to.
struct Externals {
    address compact;
    address bridge;
    uint256 collateralBps;
}

/// @dev Full deployment + configuration in a single script.
///
/// Deploy order:
///   1. SolverEscrow
///   2. Auction
///   3. RouterAllocator + composition Router (via Create3Factory) - talks to the ERC7786Bridge hub
///   4. Wire all contracts together (same-chain)
///   5. Give every contract to governance (`5_HandOver`), unless `KEEP_DEPLOYER_AS_OWNER` is true
/// The deployer owns the five contracts during steps 1-4.
///
/// The CREATE3 factory is not deployed here: it is built and deployed once from contracts/shared
/// (`mise run deploy-createx` there), so every project shares one factory address.
///
/// Required env vars:
///   DEPLOYER_PK      - deployer private key
///   CONTRACT_SALT    - salt string for deterministic deployment (default SALT_VERSION)
///   CREATE3_FACTORY_ADDRESS - CREATE3 factory deployed from contracts/shared
///   BRIDGE_ADDRESS   - deployed ERC7786Bridge (the cross-chain hub facade)
///   COMPACT_ADDRESS  - The Compact address
///   COLLATERAL_BPS   - collateral requirement in basis points (e.g. 1000 = 10%)
///   OUTBE_CHAIN_ID   - chain id of Outbe. Governance is CROSSCHAIN_GOV there and its Interchain Account elsewhere
/// Optional:
///   OUTBE_RPC        - `rpc_endpoints` alias to read the governance account (default `outbe-testnet`)
///   KEEP_DEPLOYER_AS_OWNER - skip step 5 and keep the deployer as the owner. Run `5_HandOver` later
///
/// Cross-chain wiring (remote routers) is a separate step: ConfigureRouter.s.sol.
contract DeployAll is DeployRouter, DeploySolverEscrow, DeployAuction, ConfigureAll, HandOver {
    function run() public override(DeployRouter, DeploySolverEscrow, DeployAuction, ConfigureAll, HandOver) {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        address deployer = vm.addr(deployerPrivateKey);
        Externals memory externals = Externals({
            compact: vm.envAddress("COMPACT_ADDRESS"),
            bridge: vm.envAddress("BRIDGE_ADDRESS"),
            collateralBps: vm.envOr("COLLATERAL_BPS", uint256(1000))
        });
        bool keepDeployer = vm.envOr("KEEP_DEPLOYER_AS_OWNER", false);
        // Read before the broadcast: on a chain other than Outbe this asks the Outbe node.
        address governance = keepDeployer ? address(0) : governanceOwner();

        console2.log("Salt:", salt);

        vm.startBroadcast(deployerPrivateKey);

        address factoryAddr = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        console2.log("Create3Factory:", factoryAddr);

        // Everything below is deterministic from (factory, deployer, salt). If the router already
        // exists, the whole stack is already deployed, so skip it. A re-deploy of
        // escrow/auction/allocator would waste gas, and the router's CREATE3 would revert on
        // collision anyway. Run `5_HandOver` on its own to hand over an existing stack.
        address routerAddr = Create3Factory(factoryAddr).predict(deployer, getRouterSalt(salt));
        if (routerAddr.code.length != 0) {
            console2.log("Already deployed - skipping. Router:", routerAddr);
            vm.stopBroadcast();
            return;
        }

        deployAll(factoryAddr, salt, deployer, externals, governance);

        vm.stopBroadcast();

        console2.log("=== DeployAll complete ===");
    }

    /// @dev Steps 1-5. `deployer` must be the caller of every deploy step. A zero `governance` skips the
    ///      hand-over.
    function deployAll(
        address factory,
        string memory salt,
        address deployer,
        Externals memory externals,
        address governance
    ) public returns (IntentContracts memory deployed) {
        // 1. Deploy SolverEscrow
        console2.log("[1/5] Deploy SolverEscrow...");
        (deployed.escrow, deployed.solverAllocator) = deployEscrow(externals.compact, externals.collateralBps, deployer);
        console2.log("  SolverEscrow:", deployed.escrow);

        // 2. Deploy Auction
        console2.log("[2/5] Deploy Auction...");
        deployed.auction = deployAuction(deployer);
        console2.log("  Auction:", deployed.auction);

        // 3. Deploy RouterAllocator + Router via Create3Factory
        console2.log("[3/5] Deploy RouterAllocator + Router...");
        (deployed.router, deployed.allocator) = deployRouter(
            factory, salt, externals.bridge, externals.compact, deployed.escrow, deployed.auction, deployer
        );
        console2.log("  Router:", deployed.router);

        // 4. Wire all contracts together
        console2.log("[4/5] Configure all...");
        configureAll(deployed.router, deployed.auction, deployed.escrow, deployed.allocator);

        // 5. Give every contract to governance
        if (governance == address(0)) {
            console2.log("[5/5] Hand-over skipped (KEEP_DEPLOYER_AS_OWNER). The deployer keeps every owner role.");
        } else {
            console2.log("[5/5] Hand over to governance...");
            handOver(deployer, governance, deployed);
        }
    }
}
