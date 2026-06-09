// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {ISmartAccountFactory} from "./interfaces/ISmartAccountFactory.sol";
import {IKernelFactory} from "./interfaces/kernel/IKernelFactory.sol";
import {BundleModulePlugin} from "./BundleModulePlugin.sol";
import {Kernel} from "@zerodev/kernel/Kernel.sol";
import {IValidator, IHook, IFallback, IPolicy, ISigner} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {ValidationId, CallType, PermissionId} from "@zerodev/kernel/types/Types.sol";
import {ValidatorLib} from "@zerodev/kernel/utils/ValidationTypeLib.sol";
import {ValidationManager} from "@zerodev/kernel/core/ValidationManager.sol";
import {MODULE_TYPE_FALLBACK, MODULE_TYPE_EXECUTOR, CALLTYPE_SINGLE} from "@zerodev/kernel/types/Constants.sol";

contract SmartAccountFactory is ISmartAccountFactory {
    IKernelFactory private immutable _KERNEL_FACTORY;
    IValidator private immutable _ECDSA_VALIDATOR;
    IFallback private immutable _BUNDLE_MODULE_PLUGIN;
    IHook private immutable _CALLER_HOOK;
    IHook private immutable _BUNDLE_SPEND_PROTECTOR_HOOK;
    IPolicy private immutable _WITHDRAWAL_LIMIT_POLICY;
    ISigner private immutable _ECDSA_SIGNER;
    IHook private immutable _BUNDLE_WITHDRAW_HOOK;

    /// @notice Daily withdrawal limit enforced by WithdrawalLimitPolicy (6-decimal USDC units)
    uint256 public constant DAILY_LIMIT = 1000e6;
    uint48 public constant LIMIT_INTERVAL = 1 days;

    constructor(
        address kernelFactory_,
        address ecdsaValidator_,
        address bundleModulePlugin_,
        address callerHook_,
        address bundleSpendProtectorHook_,
        address withdrawalLimitPolicy_,
        address ecdsaSigner_,
        address bundleWithdrawHook_
    ) {
        _KERNEL_FACTORY = IKernelFactory(kernelFactory_);
        _ECDSA_VALIDATOR = IValidator(ecdsaValidator_);
        _BUNDLE_MODULE_PLUGIN = IFallback(bundleModulePlugin_);
        _CALLER_HOOK = IHook(callerHook_);
        _BUNDLE_SPEND_PROTECTOR_HOOK = IHook(bundleSpendProtectorHook_);
        _WITHDRAWAL_LIMIT_POLICY = IPolicy(withdrawalLimitPolicy_);
        _ECDSA_SIGNER = ISigner(ecdsaSigner_);
        _BUNDLE_WITHDRAW_HOOK = IHook(bundleWithdrawHook_);
    }

    // ── ISmartAccountFactory getters ─────────────────────────────────────

    /// @inheritdoc ISmartAccountFactory
    function kernelFactory() external view override returns (address) {
        return address(_KERNEL_FACTORY);
    }

    /// @inheritdoc ISmartAccountFactory
    function ecdsaValidator() external view override returns (address) {
        return address(_ECDSA_VALIDATOR);
    }

    /// @inheritdoc ISmartAccountFactory
    function bundleModulePlugin() external view override returns (address) {
        return address(_BUNDLE_MODULE_PLUGIN);
    }

    /// @inheritdoc ISmartAccountFactory
    function callerHook() external view override returns (address) {
        return address(_CALLER_HOOK);
    }

    /// @inheritdoc ISmartAccountFactory
    function bundleSpendProtectorHook() external view override returns (address) {
        return address(_BUNDLE_SPEND_PROTECTOR_HOOK);
    }

    /// @inheritdoc ISmartAccountFactory
    function withdrawalLimitPolicy() external view override returns (address) {
        return address(_WITHDRAWAL_LIMIT_POLICY);
    }

    /// @inheritdoc ISmartAccountFactory
    function ecdsaSigner() external view override returns (address) {
        return address(_ECDSA_SIGNER);
    }

    /// @inheritdoc ISmartAccountFactory
    function bundleWithdrawHook() external view override returns (address) {
        return address(_BUNDLE_WITHDRAW_HOOK);
    }

    // ── Account creation ─────────────────────────────────────────────────

    function createAccount(
        address owner,
        address cca,
        address[] calldata bundleTokens,
        address[] calldata bundleSenders,
        uint256 salt //todo investigate if it's ok to expose salt here
    ) external returns (address account) {
        require(owner != address(0), "owner required");
        require(cca != address(0), "cca required");
        bytes memory initData = _initData(owner, cca, bundleTokens, bundleSenders);
        account = _KERNEL_FACTORY.createAccount(initData, bytes32(salt));
    }

    function getAccountAddress(
        address owner,
        address cca,
        address[] calldata bundleTokens,
        address[] calldata bundleSenders,
        uint256 salt
    ) external view returns (address account) {
        bytes memory initData = _initData(owner, cca, bundleTokens, bundleSenders);
        account = _KERNEL_FACTORY.getAddress(initData, bytes32(salt));
    }

    /// @dev Per-token CCA permission ID: stable key for per-token daily-limit permission.
    function _ccaPermId(address token) internal pure returns (PermissionId) {
        return PermissionId.wrap(bytes4(keccak256(abi.encode("credis.cca", token))));
    }

    function _initData(address user, address cca, address[] memory bundleTokens, address[] memory bundleSenders)
        internal
        view
        returns (bytes memory)
    {
        ValidationId valId = ValidatorLib.validatorToIdentifier(_ECDSA_VALIDATOR);

        // ECDSAValidator reads owner from first 20 bytes of validatorData
        bytes memory validatorData = abi.encodePacked(user);

        // Use BundleSpendProtectorHook as root execution hook only when bundle tokens are configured.
        IHook hook;
        bytes memory rootHookData;
        if (bundleTokens.length > 0) {
            hook = _BUNDLE_SPEND_PROTECTOR_HOOK;
            rootHookData = hex"00";
        } else {
            hook = IHook(address(0));
            rootHookData = hex"";
        }

        // Build fallback init data for BundleModulePlugin guarded by CallerHook (topUp).
        //
        // Kernel MODULE_TYPE_FALLBACK initData layout:
        //   [0:4]   selector
        //   [4:24]  hook address
        //   [24:]   abi.encode(selectorData, hookData)
        //
        // _installSelector strips selectorData[0] as CallType then calls BundleModulePlugin.onInstall(selectorData[1:])
        // _installHook strips hookData[0] as flag then calls CallerHook.onInstall(hookData[1:])
        bytes memory topUpSelectorData = bytes.concat(CallType.unwrap(CALLTYPE_SINGLE), abi.encode(bundleTokens));
        bytes memory topUpHookData = bytes.concat(CallType.unwrap(CALLTYPE_SINGLE), abi.encode(bundleSenders));
        bytes memory topUpFallbackInitData = abi.encodePacked(
            BundleModulePlugin.topUp.selector, address(_CALLER_HOOK), abi.encode(topUpSelectorData, topUpHookData)
        );

        // Executor install: no hook, empty executorData (onInstall is a no-op for empty data).
        bytes memory executorInitData = abi.encodePacked(address(0), abi.encode(hex"", hex""));

        bytes[] memory initConfig = new bytes[](4 + bundleTokens.length);

        // [0] Executor install
        initConfig[0] = abi.encodeWithSelector(
            Kernel.installModule.selector, MODULE_TYPE_EXECUTOR, address(_BUNDLE_MODULE_PLUGIN), executorInitData
        );
        // [1] Fallback for topUp (guarded by CallerHook)
        initConfig[1] = abi.encodeWithSelector(
            Kernel.installModule.selector, MODULE_TYPE_FALLBACK, address(_BUNDLE_MODULE_PLUGIN), topUpFallbackInitData
        );

        uint256 n = bundleTokens.length;
        ValidationId[] memory vIds = new ValidationId[](n);
        ValidationManager.ValidationConfig[] memory configs = new ValidationManager.ValidationConfig[](n);
        bytes[] memory validationData = new bytes[](n);
        bytes[] memory permHookData = new bytes[](n);

        for (uint256 i = 0; i < n; i++) {
            PermissionId permId = _ccaPermId(bundleTokens[i]);
            vIds[i] = ValidatorLib.permissionToIdentifier(permId);

            // nonce=1: currentNonce stays 1 throughout initialize() initConfig processing.
            configs[i] = ValidationManager.ValidationConfig({nonce: 1, hook: _BUNDLE_WITHDRAW_HOOK});

            // Policy entry: bytes2(PassFlag=0) || address(policy) || abi.encode(limit, interval, token)
            bytes memory policyEntry = bytes.concat(
                bytes2(0),
                bytes20(address(_WITHDRAWAL_LIMIT_POLICY)),
                abi.encode(DAILY_LIMIT, LIMIT_INTERVAL, bundleTokens[i])
            );
            // Signer entry: bytes2(PassFlag=0) || address(signer) || abi.encodePacked(cca)
            bytes memory signerEntry = bytes.concat(bytes2(0), bytes20(address(_ECDSA_SIGNER)), abi.encodePacked(cca));

            bytes[] memory permData = new bytes[](2);
            permData[0] = policyEntry;
            permData[1] = signerEntry;
            validationData[i] = abi.encode(permData);

            // hookData flag byte: 0x00 = call onInstall only if not yet initialized
            permHookData[i] = hex"00";
        }

        // [3] installValidations: one permission per bundle token
        initConfig[3] =
            abi.encodeWithSelector(Kernel.installValidations.selector, vIds, configs, validationData, permHookData);

        // [4..3+N] grantAccess: allow each permission to call execute.selector
        for (uint256 i = 0; i < n; i++) {
            initConfig[4 + i] =
                abi.encodeWithSelector(Kernel.grantAccess.selector, vIds[i], Kernel.execute.selector, true);
        }

        return abi.encodeWithSelector(Kernel.initialize.selector, valId, hook, validatorData, rootHookData, initConfig);
    }
}
