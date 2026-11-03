// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {UPDATE} from "@precompiles/IUpdate.sol";

/// @title UpgradeBase
/// @author Outbe
/// @notice Shared plumbing to upgrade the intex UUPS proxies in place: deploy a fresh
///         implementation and point the existing CREATE3 proxy at it via `upgradeToAndCall`.
/// @dev The proxy keeps its storage (roles, peers, balances). Only the implementation pointer
///      changes. The script locates each proxy by its deterministic CREATE3 address (same
///      `predictProxy` as the deploy scripts), so no addresses need to be passed in. `data` is
///      empty (logic-only upgrade). Pass `reinitializer` calldata here if a storage migration is
///      ever needed.
///      Before the hand-over the deployer holds the upgrade authority (DEFAULT_ADMIN_ROLE) and
///      upgrades the proxy itself. After it, governance holds it: the script only deploys the
///      implementations and prints one `upgradeContracts` payload with every proxy of this chain.
///      The vote targets the Update precompile.
abstract contract UpgradeBase is BaseScript {
    /// @dev The upgrades of the handed-over proxies, as `upgradeContracts` entries.
    string[] internal governanceUpgrades;

    /// @dev Upgrade the proxy at `prefix`'s deterministic address to `newImpl`.
    function upgradeProxy(Create3Factory factory, address deployer, string memory prefix, address newImpl) internal {
        address proxy = predictProxy(factory, deployer, prefix);
        require(proxy.code.length != 0, string.concat("proxy not deployed: ", prefix));
        if (IAccessControl(proxy).hasRole(bytes32(0), deployer)) {
            UUPSUpgradeable(proxy).upgradeToAndCall(newImpl, bytes(""));
            console.log(string.concat(prefix, " upgraded -> impl"), newImpl);
            return;
        }
        console.log(string.concat(prefix, " belongs to governance: impl"), newImpl);
        governanceUpgrades.push(
            string.concat(
                '{"chainId":',
                vm.toString(block.chainid),
                ',"proxy":"',
                vm.toString(proxy),
                '","implementation":"',
                vm.toString(newImpl),
                '","callData":"0x"}'
            )
        );
    }

    /// @dev Prints the one vote payload for every handed-over proxy of this chain. Nothing to print
    ///      if the deployer upgraded every proxy itself.
    function printVotePayload() internal view {
        if (governanceUpgrades.length == 0) return;
        string memory upgrades = governanceUpgrades[0];
        for (uint256 i = 1; i < governanceUpgrades.length; i++) {
            upgrades = string.concat(upgrades, ",", governanceUpgrades[i]);
        }
        console.log("Vote on the Update precompile:", UPDATE);
        console.log(string.concat('{"operation":"upgradeContracts","upgrades":[', upgrades, "]}"));
    }
}

/// @title UpgradeTarget
/// @notice Upgrade a target chain's intex proxies (NFT + bridge + escrow + auction + router) to
///         freshly compiled implementations.
/// @dev Env: DEPLOYER_PRIVATE_KEY (deploys the implementations, and upgrades only before the
///      hand-over), BRIDGE_ADDRESS, ORIGIN_CHAIN_ID.
///      Impl constructor args mirror DeployTarget so the immutables are unchanged.
contract UpgradeTarget is UpgradeBase {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        uint32 originChainId = uint32(vm.envUint("ORIGIN_CHAIN_ID"));

        Create3Factory factory = create3Factory();
        address nft = predictProxy(factory, deployer, "IntexNFT1155");

        vm.startBroadcast(pk);
        upgradeProxy(factory, deployer, "IntexNFT1155", address(new IntexNFT1155()));
        upgradeProxy(factory, deployer, "EscrowAdapter", address(new EscrowAdapter()));
        upgradeProxy(factory, deployer, "IntexAuction", address(new IntexAuction()));
        upgradeProxy(factory, deployer, "IntexNFT1155Bridge", address(new IntexNFT1155Bridge(nft, bridge)));
        upgradeProxy(factory, deployer, "TargetRouter", address(new TargetRouter(bridge, originChainId)));

        vm.stopBroadcast();
        printVotePayload();
    }
}

/// @title UpgradeOrigin
/// @notice Upgrade the origin engine (OriginRouter) in place. The NFT collection + bridge are a
///         target concern now (see UpgradeTarget), so the origin has only OriginRouter to upgrade.
/// @dev Env: DEPLOYER_PRIVATE_KEY, BRIDGE_ADDRESS. Impl constructor args mirror DeployOrigin.
contract UpgradeOrigin is UpgradeBase {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        address bridge = vm.envAddress("BRIDGE_ADDRESS");

        Create3Factory factory = create3Factory();

        vm.startBroadcast(pk);
        upgradeProxy(factory, deployer, "OriginRouter", address(new OriginRouter(bridge)));
        vm.stopBroadcast();
        printVotePayload();
    }
}
