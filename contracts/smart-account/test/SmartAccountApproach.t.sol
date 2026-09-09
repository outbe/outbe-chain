// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
import {BaseAATest} from "./BaseAATest.sol";
import {GuardedKernel} from "src/kernel/GuardedKernel.sol";
import {Kernel} from "src/kernel/guarded/Kernel.sol";
import {SmartAccountFactory} from "src/SmartAccountFactory.sol";
import {BundleModulePlugin} from "src/BundleModulePlugin.sol";
import {ITokenBundle} from "src/interfaces/ITokenBundle.sol";
import {MockUSD} from "src/mocks/MockUSD.sol";
import {Install} from "@zerodev/kernel/types/Structs.sol";
import {ICca} from "@precompiles/ICca.sol";

contract SmartAccountApproach is BaseAATest {
    function test_CreateWithoutRegistry_PredictionAndOwnerExecution() public {
        vm.etch(bundlePlugin.CCA_REGISTRY(), "");
        address predicted = factory.getAccountAddress(user, 0);
        address a = _account();
        assertEq(a, predicted);
        assertEq(factory.createAccount(user, 0), a);
        token.mint(a, 10);
        assertTrue(_ownerCall(a, address(token), abi.encodeCall(token.transfer, (recipient, 10))));
        assertEq(token.balanceOf(recipient), 10);
        assertEq(uint8(bundlePlugin.status(a)), 0);
    }

    function test_OpenAndPermanentlyClose() public {
        address a = _account();
        _open(a);
        assertEq(bundlePlugin.linkedCca(a), cca);
        assertTrue(_ownerCall(a, a, abi.encodeCall(GuardedKernel.closeBundle, ())));
        assertEq(uint8(bundlePlugin.status(a)), 2);
        vm.expectRevert(SmartAccountFactory.BundleNotUnopened.selector);
        factory.openBundle(a, cca, _tokens(), _senders(), 1, "");
        token.mint(a, 1);
        assertTrue(_ownerCall(a, address(token), abi.encodeCall(token.transfer, (recipient, 1))));
    }

    function test_UninstallAndReplacementBlockedWithReserve() public {
        address a = _account();
        _open(a);
        _fund(a, 10);
        assertFalse(_ownerCall(a, a, abi.encodeCall(GuardedKernel.closeBundle, ())));
        assertFalse(
            _ownerCall(
                a,
                a,
                abi.encodeWithSignature(
                    "uninstallModule(uint256,address,bytes)",
                    3,
                    address(bundlePlugin),
                    abi.encode(bytes(""), abi.encodePacked(BundleModulePlugin.bundleBalance.selector))
                )
            )
        );
        assertFalse(
            _ownerCall(
                a,
                a,
                abi.encodeWithSignature(
                    "installModule(uint256,address,bytes)", 4, address(withdrawHook), abi.encode(bytes(""), bytes(""))
                )
            )
        );
        assertEq(uint8(bundlePlugin.status(a)), 1);
        assertEq(bundlePlugin.balanceOf(a, address(token)), 20);
    }

    function test_NonFirstTokenPreventsClose() public {
        address a = _account();
        MockUSD second = new MockUSD();
        address[] memory ts = new address[](2);
        ts[0] = address(second);
        ts[1] = address(token);
        _openTokens(a, ts);
        _fund(a, 1);
        assertFalse(_ownerCall(a, a, abi.encodeCall(GuardedKernel.closeBundle, ())));
        assertTrue(_pay(a, 1));
        assertTrue(_ownerCall(a, a, abi.encodeCall(GuardedKernel.closeBundle, ())));
    }

    function test_InactiveCcaRollsBackInstallation() public {
        address a = _account();
        Install[] memory p = factory.getBundleInstallPackages(cca, _tokens(), _senders());
        bytes memory sig = _permissionSignature(_installDigest(a, 0, p), userKey);
        registry.setState(cca, ICca.State.Suspended);
        vm.expectRevert();
        factory.openBundle(a, cca, _tokens(), _senders(), 0, sig);
        assertEq(Kernel(payable(a)).nonce(0), 0);
        assertFalse(withdrawHook.installed(a));
        registry.setState(cca, ICca.State.Active);
        factory.openBundle(a, cca, _tokens(), _senders(), 0, sig);
    }

    function test_InvalidSignatureAndConfiguration() public {
        address a = _account();
        Install[] memory p = factory.getBundleInstallPackages(cca, _tokens(), _senders());
        bytes memory sig = _permissionSignature(_installDigest(a, 0, p), ccaKey);
        vm.expectRevert();
        factory.openBundle(a, cca, _tokens(), _senders(), 0, sig);
        address[] memory ts = new address[](2);
        ts[0] = address(token);
        ts[1] = address(token);
        vm.expectRevert(SmartAccountFactory.InvalidConfiguration.selector);
        factory.getBundleInstallPackages(cca, ts, _senders());
    }

    function test_DelegatecallRetiresUnopenedAndCannotDrainOpen() public {
        address a = _account();
        bytes memory callData = abi.encodePacked(
            Kernel.executeUserOp.selector,
            abi.encodeCall(
                Kernel.execute,
                (
                    bytes32(bytes1(0xff)),
                    abi.encodePacked(address(new DelegateTarget()), abi.encodeWithSignature("run()"))
                )
            )
        );
        assertTrue(_submit(_op(a, callData, factory.OWNER_PERMISSION(), userKey)));
        assertEq(uint8(bundlePlugin.status(a)), 2);
        vm.expectRevert(SmartAccountFactory.BundleNotUnopened.selector);
        factory.openBundle(a, cca, _tokens(), _senders(), 0, "");
        address b = factory.createAccount(user, 1);
        vm.deal(b, 10 ether);
        _open(b);
        _fund(b, 10);
        assertFalse(_submit(_op(b, callData, factory.OWNER_PERMISSION(), userKey)));
        assertEq(uint8(bundlePlugin.status(b)), 1);
    }

    function test_UpgradeRequiresEmptyReservesAndRetiresEligibility() public {
        address a = _account();
        _open(a);
        _fund(a, 10);
        GuardedKernel next = new GuardedKernel(entrypoint, bundlePlugin);
        bytes memory upgrade = abi.encodeWithSignature("upgradeToAndCall(address,bytes)", address(next), bytes(""));
        assertFalse(_ownerCall(a, a, upgrade));
        assertEq(uint8(bundlePlugin.status(a)), 1);
        assertTrue(_pay(a, 10));
        assertTrue(_ownerCall(a, a, upgrade));
        assertEq(uint8(bundlePlugin.status(a)), 2);
        assertFalse(withdrawHook.installed(a));
        address b = factory.createAccount(user, 1);
        vm.deal(b, 10 ether);
        assertTrue(_ownerCall(b, b, upgrade));
        assertEq(uint8(bundlePlugin.status(b)), 2);
        vm.expectRevert(SmartAccountFactory.BundleNotUnopened.selector);
        factory.openBundle(b, cca, _tokens(), _senders(), 0, "");
    }

    function test_SignedInstallRootAndSelectorChangesCannotBypassLock() public {
        address a = _account();
        _open(a);
        _fund(a, 10);
        Install[] memory packages = new Install[](1);
        packages[0] = Install(4, address(withdrawHook), "", "");
        uint256 nonce = Kernel(payable(a)).nonce(0);
        bytes memory sig = _permissionSignature(_installDigest(a, nonce, packages), userKey);
        vm.expectRevert(GuardedKernel.BundleConfigurationLocked.selector);
        Kernel(payable(a)).installModule(false, nonce, packages, sig);
        assertEq(Kernel(payable(a)).nonce(0), nonce);
        assertFalse(_ownerCall(a, a, abi.encodeWithSignature("setRoot(bytes21)", bytes21(0))));
        assertFalse(_ownerCall(a, a, abi.encodeWithSignature("grantAccess(bytes21,bytes)", bytes21(0), hex"12345678")));
        assertEq(bundlePlugin.balanceOf(a, address(token)), 20);
    }

    function test_DelegateFallbackCannotBypassReserveCheck() public {
        address a = _account();
        DelegateTarget target = new DelegateTarget();
        bytes memory install = abi.encodeWithSignature(
            "installModule(uint256,address,bytes)",
            uint256(3),
            address(target),
            abi.encode(bytes(""), abi.encodePacked(DelegateTarget.run.selector, bytes1(0xff), bytes20(address(1))))
        );
        assertTrue(_ownerCall(a, a, install));
        _open(a);
        _fund(a, 10);
        vm.expectRevert();
        DelegateTarget(a).run();
        assertEq(uint8(bundlePlugin.status(a)), 1);
        assertTrue(_pay(a, 10));
        DelegateTarget(a).run();
        assertEq(uint8(bundlePlugin.status(a)), 2);
    }

    function test_DirectRetirementCannotCloseAnotherAccount() public {
        address a = _account();
        _open(a);
        _fund(a, 10);
        bundlePlugin.retire();
        assertEq(uint8(bundlePlugin.status(a)), 1);
        vm.expectRevert();
        GuardedKernel(payable(a)).closeBundle();
    }
}

contract DelegateTarget {
    function run() external {}
    function onInstall(bytes calldata) external payable {}
    function onUninstall(bytes calldata) external payable {}

    function isModuleType(uint256 id) external pure returns (bool) {
        return id == 3;
    }
}
