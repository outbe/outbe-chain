// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {BaseScript} from "./BaseScript.s.sol";
import {Create2} from "@openzeppelin/contracts/utils/Create2.sol";
import {SmartAccountFactory} from "src/SmartAccountFactory.sol";
import {WithdrawalLimitPolicy} from "src/WithdrawalLimitPolicy.sol";
import {ExecutionDelayPolicy} from "src/ExecutionDelayPolicy.sol";

contract DeploySmartAccountStack is BaseScript {
    function run() public {
        address kernelFactory = vm.envAddress("KERNEL_FACTORY_ADDRESS");
        address signer = vm.envAddress("ECDSA_SIGNER_ADDRESS");
        vm.startBroadcast();
        address delay =
            _deploy("ExecutionDelayPolicy", "EXECUTION_DELAY_POLICY_ADDRESS", type(ExecutionDelayPolicy).creationCode);
        address withdrawal = _deploy(
            "WithdrawalLimitPolicy", "WITHDRAWAL_LIMIT_POLICY_ADDRESS", type(WithdrawalLimitPolicy).creationCode
        );
        _deploy(
            "SmartAccountFactory",
            "SMART_ACCOUNT_FACTORY_ADDRESS",
            abi.encodePacked(
                type(SmartAccountFactory).creationCode, abi.encode(kernelFactory, delay, withdrawal, signer)
            )
        );
        vm.stopBroadcast();
    }

    function _deploy(string memory name, string memory key, bytes memory code) private returns (address predicted) {
        bytes32 salt = generateSalt(name);
        predicted = Create2.computeAddress(salt, keccak256(code), CREATE2_FACTORY);
        if (predicted.code.length == 0) require(Create2.deploy(0, salt, code) == predicted, "address mismatch");
        printAndWrite(exportLine(key, vm.toString(predicted)));
    }
}
