// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;
import {BaseAATest} from "./BaseAATest.sol";
import {Kernel} from "@zerodev/kernel/Kernel.sol";
import {PackedUserOperation} from "account-abstraction/interfaces/PackedUserOperation.sol";
import {Install} from "@zerodev/kernel/types/Structs.sol";
import {SudoPolicy} from "src/kernel/SudoPolicy.sol";
import {PermissionId, ValidationId} from "@zerodev/kernel/types/Types.sol";
import {ExecutionDelayPolicy} from "src/ExecutionDelayPolicy.sol";

contract SmartAccountApproach is BaseAATest {
    address account;

    function setUp() public override {
        super.setUp();
        account = _deployAccount();
        vm.deal(account, 10 ether);
        _fund(account, 2000e6);
    }

    function _execution(address target, uint256 value, bytes memory data) private pure returns (bytes memory) {
        return abi.encodeWithSelector(Kernel.execute.selector, bytes32(0), abi.encodePacked(target, value, data));
    }

    function _send(bytes memory execution) private {
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, execution), user.privKey);
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
    }

    function _reject(bytes memory execution) private {
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, execution), user.privKey);
        vm.expectRevert();
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
    }

    function _schedule(bytes memory execution) private returns (bytes32 id) {
        _send(_execution(address(delayPolicy), 0, abi.encodeCall(delayPolicy.schedule, (execution))));
        id = delayPolicy.operationId(account, execution);
        assertEq(delayPolicy.readyAt(account, id), vm.getBlockTimestamp() + 300);
    }

    function test_DelayBoundaryAndReplay() public {
        bytes memory execution = _execution(address(token), 0, abi.encodeCall(token.transfer, (recipient.addr, 100e6)));
        bytes32 id = _schedule(execution);
        vm.warp(vm.getBlockTimestamp() + 299);
        _reject(execution);
        vm.warp(vm.getBlockTimestamp() + 1);
        _send(execution);
        assertEq(token.balanceOf(recipient.addr), 100e6);
        assertEq(delayPolicy.readyAt(account, id), 0);
        _reject(execution);
    }

    function test_ScheduleTimestampBound() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        vm.warp(type(uint48).max - 300);
        vm.prank(account);
        bytes32 id = delayPolicy.schedule(execution);
        assertEq(delayPolicy.readyAt(account, id), type(uint48).max);
        vm.prank(account);
        delayPolicy.cancel(id);
        vm.warp(type(uint48).max - 299);
        vm.expectRevert(ExecutionDelayPolicy.InvalidExecution.selector);
        vm.prank(account);
        delayPolicy.schedule(execution);
    }

    function test_CancelRescheduleAndAlteredCalldata() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        bytes32 id = _schedule(execution);
        _send(_execution(address(delayPolicy), 0, abi.encodeCall(delayPolicy.cancel, (id))));
        vm.warp(vm.getBlockTimestamp() + 300);
        _reject(execution);
        _schedule(execution);
        _reject(execution);
        vm.warp(vm.getBlockTimestamp() + 300);
        _reject(_execution(recipient.addr, 2 ether, ""));
        _send(execution);
        assertEq(recipient.addr.balance, 1 ether);
    }

    function test_FailedExecutionCanRetry() public {
        bytes memory execution = _execution(address(token), 0, abi.encodeCall(token.transfer, (recipient.addr, 3000e6)));
        bytes32 id = _schedule(execution);
        vm.warp(vm.getBlockTimestamp() + 300);
        _send(execution); // EntryPoint catches the token's insufficient-balance revert.
        assertGt(delayPolicy.readyAt(account, id), 0);
        _fund(account, 1000e6);
        _send(execution);
        assertEq(delayPolicy.readyAt(account, id), 0);
        assertEq(token.balanceOf(recipient.addr), 3000e6);
    }

    function test_RootModeCannotBypassHook() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        _schedule(execution);
        vm.warp(vm.getBlockTimestamp() + 300);
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildUserOp(account, execution, user.privKey);
        ops[0].nonce = entrypoint.getNonce(account, 0);
        ops[0].signature = _permSignature(entrypoint.getUserOpHash(ops[0]), user.privKey);
        vm.expectRevert();
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
    }

    function test_ApprovalAndSelfCallsRequireDelay() public {
        bytes memory approval = _execution(address(token), 0, abi.encodeCall(token.approve, (recipient.addr, 500e6)));
        _reject(approval);
        _schedule(approval);
        vm.warp(vm.getBlockTimestamp() + 300);
        _send(approval);
        assertEq(token.allowance(account, recipient.addr), 500e6);
        bytes memory nested = _execution(account, 0, _execution(recipient.addr, 1 ether, ""));
        _reject(nested);
        _schedule(nested);
        vm.warp(vm.getBlockTimestamp() + 300);
        _send(nested);
        assertEq(recipient.addr.balance, 1 ether);
    }

    function test_DirectOwnerCallRejectedAndSignaturesDisabled() public {
        vm.prank(user.addr);
        vm.expectRevert();
        Kernel(payable(account)).execute(bytes32(0), abi.encodePacked(recipient.addr, uint256(1), bytes("")));
        assertEq(delayPolicy.checkSignaturePolicy(bytes32(0), account, bytes32(0), ""), 1);
    }

    function test_PredictionAndEmptyTokenAccountStillDelayed() public {
        address[] memory tokens = new address[](0);
        address predicted = factory.getAccountAddress(user.addr, cca.addr, tokens, 42);
        assertEq(factory.createAccount(user.addr, cca.addr, tokens, 42), predicted);
        account = predicted;
        vm.deal(account, 10 ether);
        _reject(_execution(recipient.addr, 1 ether, ""));
    }

    function test_DuplicatePendingDoesNotRestartClock() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        bytes32 id = _schedule(execution);
        uint48 original = delayPolicy.readyAt(account, id);
        vm.warp(vm.getBlockTimestamp() + 10);
        _send(_execution(address(delayPolicy), 0, abi.encodeCall(delayPolicy.schedule, (execution))));
        assertEq(delayPolicy.readyAt(account, id), original);
    }

    function test_ChainAndGenerationBinding() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        bytes32 id = _schedule(execution);
        vm.chainId(block.chainid + 1);
        assertNotEq(delayPolicy.operationId(account, execution), id);
        _reject(execution);
        vm.prank(account);
        delayPolicy.onUninstall(abi.encodePacked(bytes32(PermissionId.unwrap(_ownerPermId()))));
        vm.prank(account);
        delayPolicy.onInstall(abi.encodePacked(bytes32(PermissionId.unwrap(_ownerPermId()))));
        assertNotEq(delayPolicy.operationId(account, execution), id);
    }

    function test_SameBundleReplayExecutesOnlyOnce() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        _schedule(execution);
        vm.warp(vm.getBlockTimestamp() + 300);
        PackedUserOperation[] memory ops = new PackedUserOperation[](2);
        ops[0] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, execution), user.privKey);
        ops[1] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, execution), user.privKey);
        ops[1].nonce++;
        ops[1].signature = _permSignature(entrypoint.getUserOpHash(ops[1]), user.privKey);
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
        assertEq(recipient.addr.balance, 1 ether);
    }

    function test_SameBundleSchedulingCannotAuthorizeExecution() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        bytes memory schedule = _execution(address(delayPolicy), 0, abi.encodeCall(delayPolicy.schedule, (execution)));
        PackedUserOperation[] memory ops = new PackedUserOperation[](2);
        ops[0] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, schedule), user.privKey);
        ops[1] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, execution), user.privKey);
        ops[1].nonce++;
        ops[1].signature = _permSignature(entrypoint.getUserOpHash(ops[1]), user.privKey);
        vm.expectRevert();
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
        assertEq(delayPolicy.readyAt(account, delayPolicy.operationId(account, execution)), 0);
    }

    function test_RootReplacementIsDelayed() public {
        SudoPolicy replacement = new SudoPolicy();
        bytes4 id = bytes4(keccak256("replacement"));
        Install[] memory packages = new Install[](2);
        packages[0] = Install(5, address(replacement), abi.encodePacked(bytes32(id)), abi.encodePacked(id));
        packages[1] = Install(
            6,
            address(ecdsaSigner),
            abi.encodePacked(bytes32(id), user.addr),
            abi.encodePacked(id, address(0), Kernel.execute.selector)
        );
        bytes memory change =
            abi.encodeWithSignature("setRoot((uint256,address,bytes,bytes)[],bool,bytes)", packages, false, bytes(""));
        bytes memory execution = _execution(account, 0, change);
        _reject(execution);
        _schedule(execution);
        vm.warp(vm.getBlockTimestamp() + 300);
        _send(execution);
        assertEq(
            ValidationId.unwrap(Kernel(payable(account)).root()),
            bytes21(abi.encodePacked(bytes1(0x02), id, bytes16(0)))
        );
    }

    function test_UpgradeRemovalAlternateValidatorAndDelegatecallCannotBypassDelay() public {
        _reject(
            _execution(
                account, 0, abi.encodeWithSignature("upgradeToAndCall(address,bytes)", recipient.addr, bytes(""))
            )
        );
        _reject(
            _execution(
                account,
                0,
                abi.encodeWithSignature(
                    "uninstallModule(uint256,address,bytes)",
                    5,
                    address(delayPolicy),
                    abi.encode(
                        abi.encodePacked(bytes32(PermissionId.unwrap(_ownerPermId()))),
                        abi.encodePacked(PermissionId.unwrap(_ownerPermId()))
                    )
                )
            )
        );
        _reject(
            _execution(
                account,
                0,
                abi.encodeWithSignature("installModule(uint256,address,bytes)", 1, recipient.addr, bytes(""))
            )
        );
        _reject(
            abi.encodeWithSelector(
                Kernel.execute.selector, bytes32(bytes1(0xff)), abi.encodePacked(recipient.addr, bytes(""))
            )
        );
    }

    function test_ERC1271AndSignedModuleInstallationRejected() public {
        bytes32 digest = keccak256("signed-message");
        bytes memory signature = abi.encodePacked(bytes2(0), _permSignature(digest, user.privKey));
        assertNotEq(Kernel(payable(account)).isValidSignature(digest, signature), bytes4(0x1626ba7e));
        Install[] memory packages = new Install[](0);
        bytes32 domain = keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256("Kernel"),
                keccak256("0.4.0"),
                block.chainid,
                account
            )
        );
        bytes32 installHash = keccak256(
            abi.encode(
                keccak256(
                    "InstallPackages(uint256 nonce,Install[] packages)Install(uint256 moduleType,address module,bytes moduleData,bytes internalData)"
                ),
                uint256(0),
                keccak256("")
            )
        );
        bytes32 installDigest = keccak256(abi.encodePacked(bytes2(0x1901), domain, installHash));
        bytes memory installSignature = _permSignature(installDigest, user.privKey);
        vm.expectRevert(abi.encodeWithSignature("InstallSignatureVerificationFailed()"));
        Kernel(payable(account)).installModule(false, 0, packages, installSignature);
    }

    function test_SameBundleCancellationPreventsReadyExecution() public {
        bytes memory execution = _execution(recipient.addr, 1 ether, "");
        bytes32 id = _schedule(execution);
        vm.warp(vm.getBlockTimestamp() + 300);
        bytes memory cancel = _execution(address(delayPolicy), 0, abi.encodeCall(delayPolicy.cancel, (id)));
        PackedUserOperation[] memory ops = new PackedUserOperation[](2);
        ops[0] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, cancel), user.privKey);
        ops[1] = _buildUserOp(account, abi.encodePacked(Kernel.executeUserOp.selector, execution), user.privKey);
        ops[1].nonce++;
        ops[1].signature = _permSignature(entrypoint.getUserOpHash(ops[1]), user.privKey);
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
        assertEq(recipient.addr.balance, 0);
        assertEq(delayPolicy.readyAt(account, id), 0);
    }
}
