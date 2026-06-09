// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {BaseScript} from "./BaseScript.s.sol";
import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {console} from "forge-std/Script.sol";

import {Kernel} from "@zerodev/kernel/Kernel.sol";
import {KernelFactory} from "@zerodev/kernel/factory/KernelFactory.sol";
import {ECDSAValidator} from "@zerodev/kernel/validator/ECDSAValidator.sol";
import {IEntryPoint} from "@zerodev/kernel/interfaces/IEntryPoint.sol";
import {EntryPointLib, ENTRYPOINT_0_7_ADDR} from "@zerodev/kernel-test/base/erc4337Util.sol";
import {CallerHook} from "src/kernel/CallerHook.sol";
import {ECDSASigner} from "src/kernel/ECDSASigner.sol";

/// @title DeployKernelStack
/// @notice Deploys ERC-4337 + ZeroDev Kernel v3.1 infrastructure
/// @dev For each contract:
///      1) Predict the CREATE2 address
///      2) Deploy it, or warn if it's already deployed
///      3) Export the address as `export NAME=0x...` and append to the deployment file
contract DeployKernelStack is BaseScript {
    // Deployed contracts
    IEntryPoint public entrypoint;
    Kernel public kernelImpl;
    KernelFactory public kernelFactory;
    ECDSAValidator public ecdsaValidator;
    CallerHook public callerHook;
    ECDSASigner public ecdsaSigner;

    function run() public {
        console.log("=== Deploying Kernel Stack ===");
        console.log("Network:", getEnvName());
        console.log("Deployer:", msg.sender);
        console.log("Salt Version:", SALT_VERSION);
        console.log("Deployment file:", deploymentFilePath());
        console.log("");

        _writeDeploymentHeader();

        vm.startBroadcast();

        _deployEntryPoint();
        console.log("");

        _deployKernelImpl();
        console.log("");

        _deployKernelFactory();
        console.log("");

        _deployECDSAValidator();
        console.log("");

        _deployCallerHook();
        console.log("");

        _deployECDSASigner();
        console.log("");

        vm.stopBroadcast();
    }

    function _writeDeploymentHeader() internal {
        console.log("ENV:");
        printAndWrite(
            string.concat(
                "# Kernel stack deployment at block ",
                vm.toString(vm.getBlockNumber()),
                " timestamp ",
                vm.toString(vm.getBlockTimestamp())
            )
        );
    }

    function _deployEntryPoint() internal {
        console.log("=== Deploying EntryPoint v0.7 ===");

        // 1) Predict (canonical address, not CREATE2-derived)
        address predicted = ENTRYPOINT_0_7_ADDR;

        // 2) Deploy or warn
        if (predicted.code.length > 0) {
            console.log("WARNING: EntryPoint already deployed at:", predicted);
            entrypoint = IEntryPoint(predicted);
        } else {
            entrypoint = IEntryPoint(EntryPointLib.deploy());
            require(address(entrypoint) == predicted, "EntryPoint address mismatch");
            console.log("EntryPoint deployed:", address(entrypoint));
        }

        // 3) Export + write
        printAndWrite(exportLine("ENTRYPOINT_ADDRESS", vm.toString(address(entrypoint))));
    }

    function _deployKernelImpl() internal {
        console.log("=== Deploying Kernel Implementation ===");

        // 1) Predict
        bytes32 salt = generateSalt("KernelImpl");
        bytes memory creationCode = abi.encodePacked(type(Kernel).creationCode, abi.encode(address(entrypoint)));
        address predicted = Create2.computeAddress(salt, keccak256(creationCode), CREATE2_FACTORY);

        // 2) Deploy or warn
        if (predicted.code.length > 0) {
            console.log("WARNING: Kernel implementation already deployed at:", predicted);
        } else {
            address deployed = Create2.deploy(0, salt, creationCode);
            require(deployed == predicted, "Kernel implementation address mismatch");
            console.log("Kernel implementation deployed:", deployed);
        }

        kernelImpl = Kernel(payable(predicted));

        // 3) Export + write
        printAndWrite(exportLine("KERNEL_ADDRESS", vm.toString(predicted)));
    }

    function _deployKernelFactory() internal {
        console.log("=== Deploying KernelFactory ===");

        // 1) Predict
        bytes32 salt = generateSalt("KernelFactory");
        bytes memory creationCode = abi.encodePacked(type(KernelFactory).creationCode, abi.encode(address(kernelImpl)));
        address predicted = Create2.computeAddress(salt, keccak256(creationCode), CREATE2_FACTORY);

        // 2) Deploy or warn
        if (predicted.code.length > 0) {
            console.log("WARNING: KernelFactory already deployed at:", predicted);
        } else {
            address deployed = Create2.deploy(0, salt, creationCode);
            require(deployed == predicted, "KernelFactory address mismatch");
            console.log("KernelFactory deployed:", deployed);
        }

        kernelFactory = KernelFactory(predicted);

        // 3) Export + write
        printAndWrite(exportLine("KERNEL_FACTORY_ADDRESS", vm.toString(predicted)));
    }

    function _deployECDSAValidator() internal {
        console.log("=== Deploying ECDSAValidator ===");

        // 1) Predict
        bytes32 salt = generateSalt("ECDSAValidator");
        bytes memory creationCode = type(ECDSAValidator).creationCode;
        address predicted = Create2.computeAddress(salt, keccak256(creationCode), CREATE2_FACTORY);

        // 2) Deploy or warn
        if (predicted.code.length > 0) {
            console.log("WARNING: ECDSAValidator already deployed at:", predicted);
        } else {
            address deployed = Create2.deploy(0, salt, creationCode);
            require(deployed == predicted, "ECDSAValidator address mismatch");
            console.log("ECDSAValidator deployed:", deployed);
        }

        ecdsaValidator = ECDSAValidator(predicted);

        // 3) Export + write
        printAndWrite(exportLine("ECDSA_VALIDATOR_ADDRESS", vm.toString(predicted)));
    }

    function _deployCallerHook() internal {
        console.log("=== Deploying CallerHook ===");

        // 1) Predict
        bytes32 salt = generateSalt("CallerHook");
        bytes memory creationCode = type(CallerHook).creationCode;
        address predicted = Create2.computeAddress(salt, keccak256(creationCode), CREATE2_FACTORY);

        // 2) Deploy or warn
        if (predicted.code.length > 0) {
            console.log("WARNING: CallerHook already deployed at:", predicted);
        } else {
            address deployed = Create2.deploy(0, salt, creationCode);
            require(deployed == predicted, "CallerHook address mismatch");
            console.log("CallerHook deployed:", deployed);
        }

        callerHook = CallerHook(predicted);

        // 3) Export + write
        printAndWrite(exportLine("CALLER_HOOK_ADDRESS", vm.toString(predicted)));
    }

    function _deployECDSASigner() internal {
        console.log("=== Deploying ECDSASigner ===");

        // 1) Predict
        bytes32 salt = generateSalt("ECDSASigner");
        bytes memory creationCode = type(ECDSASigner).creationCode;
        address predicted = Create2.computeAddress(salt, keccak256(creationCode), CREATE2_FACTORY);

        // 2) Deploy or warn
        if (predicted.code.length > 0) {
            console.log("WARNING: ECDSASigner already deployed at:", predicted);
        } else {
            address deployed = Create2.deploy(0, salt, creationCode);
            require(deployed == predicted, "ECDSASigner address mismatch");
            console.log("ECDSASigner deployed:", deployed);
        }

        ecdsaSigner = ECDSASigner(predicted);

        // 3) Export + write
        printAndWrite(exportLine("ECDSA_SIGNER_ADDRESS", vm.toString(predicted)));
    }
}
