// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
import {Test} from "forge-std/Test.sol";
import {Vm} from "forge-std/Vm.sol";
import {SmartAccountFactory} from "src/SmartAccountFactory.sol";
import {BundleModulePlugin} from "src/BundleModulePlugin.sol";
import {BundleWithdrawHook} from "src/BundleWithdrawHook.sol";
import {GuardedKernel} from "src/kernel/GuardedKernel.sol";
import {Kernel} from "src/kernel/guarded/Kernel.sol";
import {SudoPolicy} from "src/kernel/SudoPolicy.sol";
import {ECDSASigner} from "src/kernel/ECDSASigner.sol";
import {MockUSD} from "src/mocks/MockUSD.sol";
import {MockCcaRegistry} from "src/mocks/MockCcaRegistry.sol";
import {ITokenBundle} from "src/interfaces/ITokenBundle.sol";
import {KernelFactory} from "@zerodev/kernel/KernelFactory.sol";
import {KernelUUPS} from "@zerodev/kernel/KernelUUPS.sol";
import {KernelImmutableECDSA} from "@zerodev/kernel/KernelImmutableECDSA.sol";
import {Install} from "@zerodev/kernel/types/Structs.sol";
import {IEntryPoint} from "account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "account-abstraction/interfaces/PackedUserOperation.sol";
import {EntryPointLib} from "./utils/EntryPointLib.sol";

abstract contract BaseAATest is Test {
    SmartAccountFactory factory;
    BundleModulePlugin bundlePlugin;
    BundleWithdrawHook withdrawHook;
    GuardedKernel implementation;
    IEntryPoint entrypoint;
    MockUSD token;
    MockCcaRegistry registry;
    address user;
    uint256 userKey;
    address cca;
    uint256 ccaKey;
    address vault;
    address recipient;
    address constant BUNDLER = address(0xdead);

    function setUp() public virtual {
        (user, userKey) = makeAddrAndKey("user");
        (cca, ccaKey) = makeAddrAndKey("cca");
        vault = makeAddr("vault");
        recipient = makeAddr("recipient");
        entrypoint = EntryPointLib.deploy();
        bundlePlugin = new BundleModulePlugin(address(this));
        implementation = new GuardedKernel(entrypoint, bundlePlugin);
        KernelFactory kf =
            new KernelFactory(KernelUUPS(payable(address(implementation))), new KernelImmutableECDSA(entrypoint));
        withdrawHook = new BundleWithdrawHook(address(bundlePlugin));
        factory = new SmartAccountFactory(
            address(kf),
            address(new SudoPolicy()),
            address(bundlePlugin),
            address(new ECDSASigner()),
            address(withdrawHook)
        );
        bundlePlugin.setFactory(address(factory));
        token = new MockUSD();
        vm.etch(bundlePlugin.CCA_REGISTRY(), address(new MockCcaRegistry()).code);
        registry = MockCcaRegistry(bundlePlugin.CCA_REGISTRY());
    }

    function _tokens() internal view returns (address[] memory a) {
        a = new address[](1);
        a[0] = address(token);
    }

    function _senders() internal view returns (address[] memory a) {
        a = new address[](1);
        a[0] = vault;
    }

    function _account() internal returns (address a) {
        a = factory.createAccount(user, 0);
        vm.deal(a, 10 ether);
    }

    function _open(address a) internal {
        _openTokens(a, _tokens());
    }

    function _openTokens(address a, address[] memory tokens) internal {
        Install[] memory p = factory.getBundleInstallPackages(cca, tokens, _senders());
        uint256 nonce = Kernel(payable(a)).nonce(0);
        factory.openBundle(
            a, cca, tokens, _senders(), nonce, _permissionSignature(_installDigest(a, nonce, p), userKey)
        );
    }

    function _installDigest(address a, uint256 nonce, Install[] memory p) internal view returns (bytes32) {
        bytes32[] memory hashes = new bytes32[](p.length);
        for (uint256 i; i < p.length; ++i) {
            hashes[i] = keccak256(
                abi.encode(
                    keccak256("Install(uint256 moduleType,address module,bytes moduleData,bytes internalData)"),
                    p[i].moduleType,
                    p[i].module,
                    keccak256(p[i].moduleData),
                    keccak256(p[i].internalData)
                )
            );
        }
        bytes32 structHash = keccak256(
            abi.encode(
                keccak256(
                    "InstallPackages(uint256 nonce,Install[] packages)Install(uint256 moduleType,address module,bytes moduleData,bytes internalData)"
                ),
                nonce,
                keccak256(abi.encodePacked(hashes))
            )
        );
        bytes32 domain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256("Kernel"),
                keccak256("0.4.0"),
                block.chainid,
                a
            )
        );
        return keccak256(abi.encodePacked(hex"1901", domain, structHash));
    }

    function _sign(bytes32 digest, uint256 key) internal pure returns (bytes memory) {
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(key, digest);
        return abi.encodePacked(r, s, v);
    }

    function _permissionSignature(bytes32 digest, uint256 key) internal pure returns (bytes memory) {
        bytes[] memory sigs = new bytes[](2);
        sigs[0] = "";
        sigs[1] = _sign(digest, key);
        return abi.encode(sigs);
    }

    function _op(address a, bytes memory callData, bytes4 id, uint256 key)
        internal
        view
        returns (PackedUserOperation memory op)
    {
        uint192 nonceKey = (uint192(2) << 176) | (uint192(uint32(id)) << 144);
        op = PackedUserOperation(
            a,
            entrypoint.getNonce(a, nonceKey),
            "",
            callData,
            bytes32(abi.encodePacked(uint128(3_000_000), uint128(3_000_000))),
            1_000_000,
            bytes32(abi.encodePacked(uint128(1), uint128(1))),
            "",
            ""
        );
        op.signature = _permissionSignature(entrypoint.getUserOpHash(op), key);
    }

    function _submit(PackedUserOperation memory op) internal returns (bool success) {
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = op;
        vm.recordLogs();
        vm.prank(BUNDLER, BUNDLER);
        entrypoint.handleOps(ops, payable(BUNDLER));
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            if (
                logs[i].emitter == address(entrypoint)
                    && logs[i].topics[0]
                        == keccak256("UserOperationEvent(bytes32,address,address,uint256,bool,uint256,uint256)")
            ) {
                (, success,,) = abi.decode(logs[i].data, (uint256, bool, uint256, uint256));
                return success;
            }
        }
        revert("missing UserOperationEvent");
    }

    function _executeData(address target, bytes memory data) internal pure returns (bytes memory) {
        return abi.encodePacked(
            Kernel.executeUserOp.selector,
            abi.encodeCall(Kernel.execute, (bytes32(0), abi.encodePacked(target, uint256(0), data)))
        );
    }

    function _ownerCall(address a, address target, bytes memory data) internal returns (bool) {
        return _submit(_op(a, _executeData(target, data), factory.OWNER_PERMISSION(), userKey));
    }

    function _fund(address a, uint256 amount) internal {
        token.mint(a, amount);
        token.mint(vault, amount);
        assertTrue(_ownerCall(a, address(token), abi.encodeCall(token.approve, (address(bundlePlugin), amount))));
        vm.startPrank(vault);
        token.approve(address(bundlePlugin), amount);
        bundlePlugin.topUpFor(a, address(token), amount);
        vm.stopPrank();
    }

    function _payment(address a, uint256 amount) internal view returns (ITokenBundle.Payment memory) {
        return
            ITokenBundle.Payment(
                a, address(token), recipient, amount, bundlePlugin.paymentNonce(a), block.timestamp + 1 hours
            );
    }

    function _pay(address a, uint256 amount) internal returns (bool) {
        ITokenBundle.Payment memory p = _payment(a, amount);
        return _submit(
            _op(
                a,
                _executeData(
                    address(bundlePlugin),
                    abi.encodeCall(ITokenBundle.spend, (p, _sign(bundlePlugin.paymentDigest(p), ccaKey)))
                ),
                factory.ccaPermission(address(token)),
                ccaKey
            )
        );
    }
}
