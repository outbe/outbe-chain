// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;
import {BaseScript} from "./BaseScript.s.sol";
import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {BundleModulePlugin} from "src/BundleModulePlugin.sol";
import {BundleWithdrawHook} from "src/BundleWithdrawHook.sol";
import {SmartAccountFactory} from "src/SmartAccountFactory.sol";
import {GuardedKernel} from "src/kernel/GuardedKernel.sol";
import {SudoPolicy} from "src/kernel/SudoPolicy.sol";
import {KernelFactory} from "@zerodev/kernel/KernelFactory.sol";
import {KernelImmutableECDSA} from "@zerodev/kernel/KernelImmutableECDSA.sol";
import {IEntryPoint} from "account-abstraction/interfaces/IEntryPoint.sol";

contract DeploySmartAccountStack is BaseScript {
    function run() public {
        IEntryPoint ep = IEntryPoint(vm.envAddress("ENTRYPOINT_ADDRESS"));
        address signer = vm.envAddress("ECDSA_SIGNER_ADDRESS");
        KernelImmutableECDSA immutableEcdsa =
            KernelImmutableECDSA(payable(vm.envAddress("KERNEL_IMMUTABLE_ECDSA_ADDRESS")));
        vm.startBroadcast();
        BundleModulePlugin plugin = BundleModulePlugin(
            _deploy("BundleCustody", abi.encodePacked(type(BundleModulePlugin).creationCode, abi.encode(msg.sender)))
        );
        GuardedKernel implementation = GuardedKernel(
            payable(_deploy(
                    "GuardedKernel", abi.encodePacked(type(GuardedKernel).creationCode, abi.encode(ep, plugin))
                ))
        );
        KernelFactory kf = KernelFactory(
            _deploy(
                "GuardedKernelFactory",
                abi.encodePacked(type(KernelFactory).creationCode, abi.encode(implementation, immutableEcdsa))
            )
        );
        address sudo = _deploy("SudoPolicy", type(SudoPolicy).creationCode);
        address hook =
            _deploy("BundleWithdrawHook", abi.encodePacked(type(BundleWithdrawHook).creationCode, abi.encode(plugin)));
        SmartAccountFactory factory = SmartAccountFactory(
            _deploy(
                "SmartAccountFactory",
                abi.encodePacked(type(SmartAccountFactory).creationCode, abi.encode(kf, sudo, plugin, signer, hook))
            )
        );
        if (plugin.factory() == address(0)) plugin.setFactory(address(factory));
        require(plugin.factory() == address(factory), "custody factory mismatch");
        vm.stopBroadcast();
        printAndWrite(exportLine("BUNDLE_MODULE_PLUGIN_ADDRESS", vm.toString(address(plugin))));
        printAndWrite(exportLine("KERNEL_UUPS_ADDRESS", vm.toString(address(implementation))));
        printAndWrite(exportLine("KERNEL_FACTORY_ADDRESS", vm.toString(address(kf))));
        printAndWrite(exportLine("SUDO_POLICY_ADDRESS", vm.toString(sudo)));
        printAndWrite(exportLine("BUNDLE_WITHDRAW_HOOK_ADDRESS", vm.toString(hook)));
        printAndWrite(exportLine("SMART_ACCOUNT_FACTORY_ADDRESS", vm.toString(address(factory))));
    }

    function _deploy(string memory name, bytes memory code) private returns (address predicted) {
        bytes32 salt = generateSalt(name);
        predicted = Create2.computeAddress(salt, keccak256(code), CREATE2_FACTORY);
        if (predicted.code.length == 0) require(Create2.deploy(0, salt, code) == predicted, "CREATE2 mismatch");
    }
}
