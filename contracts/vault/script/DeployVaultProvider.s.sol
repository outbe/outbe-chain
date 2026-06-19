// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity 0.8.30;

import {console} from "forge-std/Script.sol";
import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {VaultProvider} from "../src/VaultProvider.sol";
import {IVaultProvider} from "../src/interfaces/IVaultProvider.sol";
import {IVaultV2} from "../src/interfaces/IVaultV2.sol";

/// @notice Minimal deterministic deploy for VaultProvider (impl + UUPS proxy via ERC1967Proxy).
/// @dev Env:
///      PRIVATE_KEY                   - deployer private key (required by BaseScript)
///      OWNER_ADDRESS                 - owner for initialize(owner)
///      VAULT_ADDRESS                 - reserve vault (asset is read from the vault)
///      CREDIS_FACTORY_ADAPTER_ADDRESS - adapter registered as both a CredisAnadosis liquidity
///                                       source and a Credis liquidity target
///      GEM_FACTORY_ADDRESS           - gem factory precompile registered as a
///                                       GemSettle liquidity source (deposit-only)
contract DeployVaultProvider is BaseScript {
    function run() external returns (address proxy) {
        address reserveVault = vm.envAddress("VAULT_ADDRESS");
        address credisFactory = vm.envAddress("CREDIS_FACTORY_ADDRESS");
        address gemFactory = vm.envAddress("GEM_FACTORY_ADDRESS");
        address intexFactory = vm.envAddress("INTEX_FACTORY_ADDRESS");

        bytes32 saltImpl = generateSalt("VaultProvider");
        bytes32 saltProxy = generateSalt("IVaultProvider");

        bytes memory implCreationCode = type(VaultProvider).creationCode;
        address impl = Create2.computeAddress(saltImpl, keccak256(implCreationCode), CREATE2_FACTORY);

        // OZ v5.6 ERC1967Proxy mandates initialization during construction: empty `_data`
        // reverts with ERC1967ProxyUninitialized(). Embed the initialize() call in the
        // proxy constructor data instead of issuing a separate post-deploy initialize tx.
        bytes memory initData = abi.encodeCall(VaultProvider.initialize, (owner));
        bytes memory proxyCreationCode = abi.encodePacked(type(ERC1967Proxy).creationCode, abi.encode(impl, initData));
        proxy = Create2.computeAddress(saltProxy, keccak256(proxyCreationCode), CREATE2_FACTORY);

        vm.startBroadcast(privateKey);
        if (impl.code.length == 0) {
            Create2.deploy(0, saltImpl, implCreationCode);
        }
        if (proxy.code.length == 0) {
            Create2.deploy(0, saltProxy, proxyCreationCode);
        }

        VaultProvider provider = VaultProvider(proxy);
        if (_isVaultRegistered(provider, reserveVault)) {
            console.log("WARN: vault already added, skipping:", reserveVault);
        } else {
            provider.addVault(reserveVault);
            console.log("Vault added:", reserveVault);
        }
        provider.addLiquiditySource(credisFactory, IVaultProvider.LiquiditySource.CredisAnadosis);
        provider.addLiquidityTarget(credisFactory, IVaultProvider.LiquidityTarget.Credis);
        provider.addLiquiditySource(gemFactory, IVaultProvider.LiquiditySource.GemSettle);
        provider.addLiquiditySource(intexFactory, IVaultProvider.LiquiditySource.IntexStrikePrice);

        vm.stopBroadcast();

        console.log("=== VaultProvider Deployment ===");
        console.log("Network:", getEnvName());
        console.log("Owner:", owner);
        console.log("Implementation:", impl);
        console.log("Proxy:", proxy);

        printAndWrite(
            string.concat(
                "# VaultProvider deployment at block ",
                vm.toString(vm.getBlockNumber()),
                " timestamp ",
                vm.toString(vm.getBlockTimestamp())
            )
        );
        printAndWrite(exportLine("VAULT_PROVIDER_IMPL_ADDRESS", vm.toString(impl)));
        printAndWrite(exportLine("VAULT_PROVIDER_ADDRESS", vm.toString(proxy)));
    }

    /// @dev Returns true when `vault` is already registered under its `asset()` in `provider`.
    ///      Used to skip `addVault` on re-runs so the broadcast batch does not include
    ///      a call that would revert with `ReserveVaultAlreadyAdded()`.
    function _isVaultRegistered(VaultProvider provider, address vault) internal view returns (bool) {
        if (vault.code.length == 0) return false;
        address asset = IVaultV2(vault).asset();
        uint256 count = provider.assetVaultsCount(asset);
        for (uint256 i = 0; i < count; i++) {
            if (provider.assetVaultAt(asset, i) == vault) {
                return true;
            }
        }
        return false;
    }
}
