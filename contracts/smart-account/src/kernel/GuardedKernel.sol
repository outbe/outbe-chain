// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;
import {Kernel} from "./guarded/Kernel.sol";
import {IEntryPoint} from "account-abstraction/interfaces/IEntryPoint.sol";
import {Install} from "@zerodev/kernel/types/Structs.sol";
import {UUPSUpgradeable} from "solady/utils/UUPSUpgradeable.sol";
import {Initializable} from "solady/utils/Initializable.sol";
import {
    MODULE_TYPE_POLICY,
    MODULE_TYPE_SIGNER,
    MODULE_TYPE_FALLBACK,
    MODULE_TYPE_HOOK
} from "@zerodev/kernel/types/Constants.sol";
import {BundleModulePlugin} from "../BundleModulePlugin.sol";
import {ITokenBundle} from "../interfaces/ITokenBundle.sol";
import {ISmartAccountFactory} from "../interfaces/ISmartAccountFactory.sol";

/// @notice Kernel v4 with custody-backed lifecycle checks at shared mutation boundaries.
contract GuardedKernel is Kernel, UUPSUpgradeable, Initializable {
    BundleModulePlugin public immutable custody;
    error BundleConfigurationLocked();

    constructor(IEntryPoint entryPoint, BundleModulePlugin custody_) Kernel(entryPoint) {
        custody = custody_;
        _disableInitializers();
    }

    function initialize(Install[] calldata packages) external payable override initializer {
        _initialize(packages);
    }

    function closeBundle() external {
        _onlyEntryPointOrSelf();
        _retire();
    }

    function _beforeModuleMutation() internal view override {
        require(custody.status(address(this)) != ITokenBundle.Status.Open, BundleConfigurationLocked());
    }

    function _beforeExternalDelegateCall() internal override {
        _retire();
    }

    function _authorizeUpgrade(address) internal override {
        _onlyEntryPointOrSelf();
        _retire();
    }

    function _retire() private {
        bool wasOpen = custody.status(address(this)) == ITokenBundle.Status.Open;
        custody.retire(); // Checks every token before making closure permanent.
        if (!wasOpen) return;
        ISmartAccountFactory factory = ISmartAccountFactory(custody.factory());
        address[] memory tokens = custody.bundleTokensOf(address(this));
        for (uint256 i; i < tokens.length; ++i) {
            bytes4 id = bytes4(keccak256(abi.encode("credis.cca", tokens[i])));
            bytes memory data = abi.encode(abi.encodePacked(bytes32(id)), abi.encodePacked(id));
            this.uninstallModule(MODULE_TYPE_POLICY, factory.sudoPolicy(), data);
            this.uninstallModule(MODULE_TYPE_SIGNER, factory.ecdsaSigner(), data);
        }
        this.uninstallModule(
            MODULE_TYPE_FALLBACK,
            address(custody),
            abi.encode(bytes(""), abi.encodePacked(BundleModulePlugin.bundleBalance.selector))
        );
        this.uninstallModule(MODULE_TYPE_HOOK, factory.bundleWithdrawHook(), abi.encode(bytes(""), bytes("")));
    }
}
