// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@contracts/factory/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";

/// @title UpgradeBase
/// @author Outbe
/// @notice Shared plumbing to upgrade the intex UUPS proxies in place: deploy a fresh
///         implementation and point the existing CREATE3 proxy at it via `upgradeToAndCall`.
/// @dev The proxy keeps its storage (roles, peers, balances) — only the implementation pointer
///      changes. Proxies are located by their deterministic CREATE3 address (same `predictProxy`
///      as the deploy scripts), so no addresses need to be passed in. The broadcaster must hold
///      each contract's upgrade authority (DEFAULT_ADMIN_ROLE for the AccessControl contracts,
///      owner for the OApp contracts) — the deployer does. `data` is empty (logic-only upgrade);
///      pass `reinitializer` calldata here if a storage migration is ever needed.
abstract contract UpgradeBase is BaseScript {
    /// @dev The CREATE3 factory at its deterministic address; reverts if not yet deployed.
    function resolveFactory() internal returns (Create3Factory) {
        address f =
            vm.computeCreate2Address(FACTORY_SALT, keccak256(type(Create3Factory).creationCode), CREATE2_FACTORY);
        require(f.code.length != 0, "Create3Factory not deployed - run the deploy first");
        return Create3Factory(f);
    }

    /// @dev Upgrade the proxy at `prefix`'s deterministic address to `newImpl`.
    function upgradeProxy(Create3Factory factory, address deployer, string memory prefix, address newImpl) internal {
        address proxy = predictProxy(factory, deployer, prefix);
        require(proxy.code.length != 0, string.concat("proxy not deployed: ", prefix));
        UUPSUpgradeable(proxy).upgradeToAndCall(newImpl, bytes(""));
        console.log(string.concat(prefix, " upgraded -> impl"), newImpl);
    }
}

/// @title UpgradeBsc
/// @notice Upgrade the BNB-side intex proxies to freshly compiled implementations.
/// @dev Env: DEPLOYER_PRIVATE_KEY (holds the upgrade authority), LZ_ENDPOINT, OUTBE_EID.
///      Impl constructor args mirror DeployBsc so the immutables are unchanged.
contract UpgradeBsc is UpgradeBase {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        address lzEndpoint = vm.envAddress("LZ_ENDPOINT");
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));

        Create3Factory factory = resolveFactory();
        address nft = predictProxy(factory, deployer, "IntexNFT1155");

        vm.startBroadcast(pk);
        upgradeProxy(factory, deployer, "IntexNFT1155", address(new IntexNFT1155()));
        upgradeProxy(factory, deployer, "EscrowAdapter", address(new EscrowAdapter()));
        upgradeProxy(factory, deployer, "IntexAuction", address(new IntexAuction()));
        upgradeProxy(factory, deployer, "ONFT1155Adapter", address(new ONFT1155Adapter(nft, lzEndpoint)));
        upgradeProxy(factory, deployer, "ONFT1155AdapterBatch", address(new ONFT1155AdapterBatch(nft, lzEndpoint)));
        upgradeProxy(factory, deployer, "TargetMessenger", address(new TargetMessenger(lzEndpoint, outbeEid)));
        vm.stopBroadcast();
    }
}

/// @title UpgradeOutbe
/// @notice Upgrade the Outbe-side intex proxies to freshly compiled implementations.
/// @dev Env: DEPLOYER_PRIVATE_KEY, LZ_ENDPOINT, BNB_EID. Impl constructor args mirror DeployOutbe.
contract UpgradeOutbe is UpgradeBase {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        address lzEndpoint = vm.envAddress("LZ_ENDPOINT");
        uint32 bnbEid = uint32(vm.envUint("BNB_EID"));

        Create3Factory factory = resolveFactory();
        address nft = predictProxy(factory, deployer, "IntexNFT1155");

        vm.startBroadcast(pk);
        upgradeProxy(factory, deployer, "IntexNFT1155", address(new IntexNFT1155()));
        upgradeProxy(factory, deployer, "ONFT1155Adapter", address(new ONFT1155Adapter(nft, lzEndpoint)));
        upgradeProxy(factory, deployer, "ONFT1155AdapterBatch", address(new ONFT1155AdapterBatch(nft, lzEndpoint)));
        upgradeProxy(factory, deployer, "OriginMessenger", address(new OriginMessenger(lzEndpoint, bnbEid)));
        vm.stopBroadcast();
    }
}

/// @title UpgradeOriginMessenger
/// @notice Upgrade only the OriginMessenger proxy in place (UUPS), leaving the other proxies untouched.
/// @dev Env: DEPLOYER_PRIVATE_KEY, LZ_ENDPOINT, BNB_EID.
contract UpgradeOriginMessenger is UpgradeBase {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        address lzEndpoint = vm.envAddress("LZ_ENDPOINT");
        uint32 bnbEid = uint32(vm.envUint("BNB_EID"));

        Create3Factory factory = resolveFactory();

        vm.startBroadcast(pk);
        upgradeProxy(factory, deployer, "OriginMessenger", address(new OriginMessenger(lzEndpoint, bnbEid)));
        vm.stopBroadcast();
    }
}
