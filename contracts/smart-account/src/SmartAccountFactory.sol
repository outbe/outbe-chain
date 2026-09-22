// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {ICcaRegistry} from "@precompiles/ICcaRegistry.sol";
import {ISmartAccountFactory} from "./interfaces/ISmartAccountFactory.sol";
import {IKernelFactory} from "./interfaces/kernel/IKernelFactory.sol";
import {IERC7579Account} from "@zerodev/kernel/interfaces/IERC7579Account.sol";
import {Install} from "@zerodev/kernel/types/Structs.sol";

/// @notice Fresh Kernel v4 accounts with delayed owner execution and immediate capped CCA transfers.
contract SmartAccountFactory is ISmartAccountFactory {
    address public immutable kernelFactory;
    address public immutable executionDelayPolicy;
    address public immutable withdrawalLimitPolicy;
    address public immutable ecdsaSigner;
    uint256 public constant DAILY_LIMIT = 1000e6;
    uint48 public constant LIMIT_INTERVAL = 1 days;
    address public constant CCA_REGISTRY = 0x0000000000000000000000000000000000001011;

    constructor(
        address kernelFactory_,
        address executionDelayPolicy_,
        address withdrawalLimitPolicy_,
        address ecdsaSigner_
    ) {
        kernelFactory = kernelFactory_;
        executionDelayPolicy = executionDelayPolicy_;
        withdrawalLimitPolicy = withdrawalLimitPolicy_;
        ecdsaSigner = ecdsaSigner_;
    }

    function createAccount(address owner, address cca, address[] calldata withdrawalTokens, uint256 salt)
        external
        returns (address)
    {
        require(owner != address(0) && cca != address(0), "owner and cca required");
        ICcaRegistry.State state = ICcaRegistry(CCA_REGISTRY).getCcaState(cca);
        require(state == ICcaRegistry.State.Active, ICcaRegistry.CcaNotActive(cca, state));
        return IKernelFactory(kernelFactory).deploy(_packages(owner, cca, withdrawalTokens), salt);
    }

    function getAccountAddress(address owner, address cca, address[] calldata withdrawalTokens, uint256 salt)
        external
        view
        returns (address)
    {
        return IKernelFactory(kernelFactory).getAddress(_packages(owner, cca, withdrawalTokens), salt);
    }

    function _packages(address owner, address cca, address[] memory tokens)
        private
        view
        returns (Install[] memory packages)
    {
        bytes4 ownerId = bytes4(keccak256("credis.owner"));
        packages = new Install[](3 + 2 * tokens.length);
        // The policy establishes the root permission; enable its hook before installing the signer.
        packages[0] = Install(5, executionDelayPolicy, abi.encodePacked(bytes32(ownerId)), abi.encodePacked(ownerId));
        packages[1] = Install(4, executionDelayPolicy, "", "");
        packages[2] = Install(
            6,
            ecdsaSigner,
            abi.encodePacked(bytes32(ownerId), owner),
            abi.encodePacked(ownerId, executionDelayPolicy, IERC7579Account.execute.selector)
        );
        for (uint256 i; i < tokens.length; ++i) {
            require(tokens[i] != address(0), "token required");
            bytes4 id = bytes4(keccak256(abi.encode("credis.cca", tokens[i])));
            packages[3 + 2 * i] = Install(
                5,
                withdrawalLimitPolicy,
                abi.encodePacked(bytes32(id), abi.encode(DAILY_LIMIT, LIMIT_INTERVAL, tokens[i])),
                abi.encodePacked(id)
            );
            packages[4 + 2 * i] = Install(
                6,
                ecdsaSigner,
                abi.encodePacked(bytes32(id), cca),
                abi.encodePacked(id, address(0), IERC7579Account.execute.selector)
            );
        }
    }
}
