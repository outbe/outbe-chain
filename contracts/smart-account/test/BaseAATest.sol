// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {SmartAccountFactory} from "src/SmartAccountFactory.sol";
import {BundleModulePlugin} from "src/BundleModulePlugin.sol";
import {CallerHook} from "src/kernel/CallerHook.sol";
import {BundleSpendProtectorHook} from "src/BundleSpendProtectorHook.sol";
import {BundleWithdrawHook} from "src/BundleWithdrawHook.sol";
import {ECDSASigner} from "src/kernel/ECDSASigner.sol";
import {WithdrawalLimitPolicy} from "src/WithdrawalLimitPolicy.sol";
import {ITokenBundle} from "src/interfaces/ITokenBundle.sol";
import {MockUSD} from "src/mocks/MockUSD.sol";
import {EntryPointLib} from "@zerodev/kernel-test/base/erc4337Util.sol";
import {Kernel} from "@zerodev/kernel/Kernel.sol";
import {KernelFactory} from "@zerodev/kernel/factory/KernelFactory.sol";
import {IValidator} from "@zerodev/kernel/interfaces/IERC7579Modules.sol";
import {IEntryPoint} from "@zerodev/kernel/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@zerodev/kernel/interfaces/PackedUserOperation.sol";
import {
    VALIDATION_MODE_DEFAULT,
    VALIDATION_TYPE_ROOT,
    VALIDATION_TYPE_PERMISSION,
    CALLTYPE_SINGLE,
    EXECTYPE_DEFAULT
} from "@zerodev/kernel/types/Constants.sol";
import {
    ValidationMode,
    ValidationType,
    ExecMode,
    ExecModeSelector,
    ExecModePayload,
    PermissionId
} from "@zerodev/kernel/types/Types.sol";
import {ExecLib} from "@zerodev/kernel/utils/ExecLib.sol";
import {ValidatorLib} from "@zerodev/kernel/utils/ValidationTypeLib.sol";
import {ECDSAValidator} from "@zerodev/kernel/validator/ECDSAValidator.sol";
import {ECDSA} from "solady/utils/ECDSA.sol";

abstract contract BaseAATest is Test {
    struct EOA {
        address addr;
        uint256 privKey;
    }

    // actors
    EOA user;
    EOA cca;
    EOA recipient;
    address vault;

    // contracts from Kernel stack
    IEntryPoint entrypoint;
    SmartAccountFactory factory;

    // plugin contracts
    BundleModulePlugin bundlePlugin;
    CallerHook bundleCallerHook;
    BundleSpendProtectorHook bundleSpendProtectorHook;
    WithdrawalLimitPolicy withdrawalLimitPolicy;
    ECDSASigner ecdsaSigner;
    BundleWithdrawHook bundleWithdrawHook;

    // token
    MockUSD token;

    address ENTRYPOINT_BENEFICIARY = address(0xdeadbeef);

    function setUp() public virtual {
        _setupEoa();

        entrypoint = IEntryPoint(EntryPointLib.deploy());
        Kernel impl = new Kernel(entrypoint);
        KernelFactory kf = new KernelFactory(address(impl));
        IValidator ecdsaValidator = new ECDSAValidator();
        bundlePlugin = new BundleModulePlugin();
        bundleCallerHook = new CallerHook();
        bundleSpendProtectorHook = new BundleSpendProtectorHook(address(bundlePlugin));
        withdrawalLimitPolicy = new WithdrawalLimitPolicy();
        ecdsaSigner = new ECDSASigner();
        bundleWithdrawHook = new BundleWithdrawHook(address(bundlePlugin));

        factory = new SmartAccountFactory(
            address(kf),
            address(ecdsaValidator),
            address(bundlePlugin),
            address(bundleCallerHook),
            address(bundleSpendProtectorHook),
            address(withdrawalLimitPolicy),
            address(ecdsaSigner),
            address(bundleWithdrawHook)
        );

        token = new MockUSD();
    }

    // -------------------------------------------------------------------------
    // Shared helpers
    // -------------------------------------------------------------------------

    function _setupEoa() private {
        (address addr, uint256 key) = makeAddrAndKey("user");
        user.addr = addr;
        user.privKey = key;

        (addr, key) = makeAddrAndKey("cca");
        cca.addr = addr;
        cca.privKey = key;

        (addr, key) = makeAddrAndKey("recipient");
        recipient.addr = addr;
        recipient.privKey = key;

        vault = makeAddr("vault");
    }

    function _topUp(address smartAccount, uint256 amount) internal {
        // Pre-fund smart account with user's own funds (required by topUp check)
        token.mint(smartAccount, amount);
        // Vault tops up matching amount
        token.mint(vault, amount);
        vm.startPrank(vault);
        token.approve(smartAccount, amount);
        ITokenBundle(smartAccount).topUp(vault, address(token), amount);
        vm.stopPrank();
    }

    function _deployAccount() internal returns (address) {
        address[] memory bundleTokens = new address[](1);
        bundleTokens[0] = address(token);

        address[] memory bundleSenders = new address[](1);
        bundleSenders[0] = vault;

        return factory.createAccount(user.addr, cca.addr, bundleTokens, bundleSenders, 0);
    }

    function _ccaPermId(address tok) internal pure returns (PermissionId) {
        return PermissionId.wrap(bytes4(keccak256(abi.encode("credis.cca", tok))));
    }

    function _buildCcaUserOp(address smartAccount, address to, uint256 amount)
        internal
        view
        returns (PackedUserOperation memory op)
    {
        ExecMode execMode = ExecLib.encode(
            CALLTYPE_SINGLE, EXECTYPE_DEFAULT, ExecModeSelector.wrap(0x00), ExecModePayload.wrap(0x00)
        );
        bytes memory transferCall = abi.encodeWithSelector(token.transfer.selector, to, amount);
        bytes memory innerExecute = abi.encodeWithSelector(
            Kernel.execute.selector, execMode, ExecLib.encodeSingle(address(token), 0, transferCall)
        );
        bytes memory callData = abi.encodePacked(Kernel.executeUserOp.selector, innerExecute);
        return _buildCcaUserOpRaw(smartAccount, callData, address(token));
    }

    function _buildCcaUserOpRaw(address smartAccount, bytes memory callData, address tok)
        internal
        view
        returns (PackedUserOperation memory op)
    {
        PermissionId permId = _ccaPermId(tok);

        uint192 nonceKey = ValidatorLib.encodeAsNonceKey(
            ValidationMode.unwrap(VALIDATION_MODE_DEFAULT),
            ValidationType.unwrap(VALIDATION_TYPE_PERMISSION),
            bytes20(PermissionId.unwrap(permId)),
            0 // parallel key
        );

        op = PackedUserOperation({
            sender: smartAccount,
            nonce: entrypoint.getNonce(smartAccount, nonceKey),
            initCode: hex"",
            callData: callData,
            accountGasLimits: bytes32(abi.encodePacked(uint128(2_000_000), uint128(2_000_000))),
            preVerificationGas: 1_000_000,
            gasFees: bytes32(abi.encodePacked(uint128(1), uint128(1))),
            paymasterAndData: hex"",
            signature: hex""
        });

        bytes32 userOpHash = entrypoint.getUserOpHash(op);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(cca.privKey, ECDSA.toEthSignedMessageHash(userOpHash));
        // Signature format: 0xFF (signer prefix, skipping policy sig) || ECDSA sig
        op.signature = abi.encodePacked(bytes1(0xFF), r, s, v);
    }

    function _ccaWithdraw(address smartAccount, address to, uint256 amount) internal {
        _ccaWithdrawToken(smartAccount, to, amount, address(token));
    }

    function _ccaWithdrawToken(address smartAccount, address to, uint256 amount, address tok) internal {
        ExecMode execMode =
            ExecLib.encode(CALLTYPE_SINGLE, EXECTYPE_DEFAULT, ExecModeSelector.wrap(0x00), ExecModePayload.wrap(0x00));
        bytes memory transferCall = abi.encodeWithSelector(MockUSD(tok).transfer.selector, to, amount);
        bytes memory innerExecute =
            abi.encodeWithSelector(Kernel.execute.selector, execMode, ExecLib.encodeSingle(tok, 0, transferCall));
        bytes memory callData = abi.encodePacked(Kernel.executeUserOp.selector, innerExecute);

        PackedUserOperation memory op = _buildCcaUserOpRaw(smartAccount, callData, tok);
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = op;
        entrypoint.handleOps(ops, payable(ENTRYPOINT_BENEFICIARY));
    }

    function _buildUserOp(address sender, bytes memory callData, uint256 signerKey)
        internal
        view
        returns (PackedUserOperation memory op)
    {
        uint192 encodedAsNonceKey = ValidatorLib.encodeAsNonceKey(
            ValidationMode.unwrap(VALIDATION_MODE_DEFAULT),
            ValidationType.unwrap(VALIDATION_TYPE_ROOT),
            bytes20(factory.ecdsaValidator()),
            0 // parallel key
        );

        op = PackedUserOperation({
            sender: sender,
            nonce: entrypoint.getNonce(sender, encodedAsNonceKey),
            initCode: hex"",
            callData: callData,
            accountGasLimits: bytes32(abi.encodePacked(uint128(1_000_000), uint128(1_000_000))),
            preVerificationGas: 1_000_000,
            gasFees: bytes32(abi.encodePacked(uint128(1), uint128(1))),
            paymasterAndData: hex"",
            signature: hex""
        });
        bytes32 userOpHash = entrypoint.getUserOpHash(op);
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(signerKey, ECDSA.toEthSignedMessageHash(userOpHash));
        op.signature = abi.encodePacked(r, s, v);
    }

    function _execMode() internal pure returns (ExecMode) {
        return
            ExecLib.encode(CALLTYPE_SINGLE, EXECTYPE_DEFAULT, ExecModeSelector.wrap(0x00), ExecModePayload.wrap(0x00));
    }
}
