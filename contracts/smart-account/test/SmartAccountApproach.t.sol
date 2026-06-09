// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {BaseAATest} from "./BaseAATest.sol";
import {CallerHook} from "src/kernel/CallerHook.sol";
import {ITokenBundle} from "src/interfaces/ITokenBundle.sol";
import {Kernel} from "@zerodev/kernel/Kernel.sol";
import {IEntryPoint} from "@zerodev/kernel/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "@zerodev/kernel/interfaces/PackedUserOperation.sol";
import {ExecLib} from "@zerodev/kernel/utils/ExecLib.sol";

contract SmartAccountApproach is BaseAATest {
    function test_UserCanSendEth_CcaCantWithdraw() external {
        // Deploy smart account
        address smartAccount = factory.createAccount(user.addr, cca.addr, new address[](0), new address[](0), 0);

        // Prefill user with 7 ETH
        vm.deal(user.addr, 7 ether);

        // User sends 1.3 ETH to smart account
        vm.prank(user.addr);
        (bool ok,) = payable(smartAccount).call{value: 1.3 ether}("");
        require(ok, "ETH transfer to smart account failed");

        assertEq(user.addr.balance, 5.7 ether, "user should have 5.7 ETH after sending to SA");
        assertEq(address(smartAccount).balance, 1.3 ether, "smart account should have 1.3 ETH");

        // Perform UserOp: send 1 ETH from smart account to recipient, signed by user
        bytes memory callData = abi.encodeWithSelector(
            Kernel.execute.selector, _execMode(), ExecLib.encodeSingle(recipient.addr, 1 ether, hex"")
        );

        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildUserOp(smartAccount, callData, user.privKey);
        entrypoint.handleOps(ops, payable(ENTRYPOINT_BENEFICIARY));

        // Check final balances
        assertEq(user.addr.balance, 5.7 ether, "user EOA should still have 5.7 ETH");
        assertApproxEqAbs(address(smartAccount).balance, 0.3 ether, 0.01 ether, "smart account should have ~0.3 ETH");
        assertEq(recipient.addr.balance, 1 ether, "recipient should have 1 ETH");

        // Malicious check: CCA cannot withdraw from smart account
        bytes memory maliciousCallData = abi.encodeWithSelector(
            Kernel.execute.selector, _execMode(), ExecLib.encodeSingle(cca.addr, address(smartAccount).balance, hex"")
        );

        PackedUserOperation[] memory maliciousOps = new PackedUserOperation[](1);
        maliciousOps[0] = _buildUserOp(smartAccount, maliciousCallData, cca.privKey);

        vm.expectRevert(abi.encodeWithSelector(IEntryPoint.FailedOp.selector, 0, "AA24 signature error"));
        entrypoint.handleOps(maliciousOps, payable(ENTRYPOINT_BENEFICIARY));
    }

    function test_UserCanTopUpErc20_AndWithdrawToRecipient() external {
        // Deploy smart account and fund it with ETH for gas
        address smartAccount = factory.createAccount(user.addr, cca.addr, new address[](0), new address[](0), 0);
        vm.deal(smartAccount, 0.1 ether);

        // Mint 1000 tokens to user
        token.mint(user.addr, 1000e18);

        // User transfers 500 tokens to smart account
        vm.prank(user.addr);
        require(token.transfer(smartAccount, 500e18), "user->SA transfer failed");

        assertEq(token.balanceOf(user.addr), 500e18, "user should have 500 tokens");
        assertEq(token.balanceOf(smartAccount), 500e18, "smart account should have 500 tokens");

        // Perform UserOp: transfer 300 tokens from smart account to recipient, signed by user
        bytes memory callData = abi.encodeWithSelector(
            Kernel.execute.selector,
            _execMode(),
            ExecLib.encodeSingle(
                address(token), 0, abi.encodeWithSelector(token.transfer.selector, recipient.addr, 300e18)
            )
        );

        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildUserOp(smartAccount, callData, user.privKey);
        entrypoint.handleOps(ops, payable(ENTRYPOINT_BENEFICIARY));

        // Check final token balances
        assertEq(token.balanceOf(user.addr), 500e18, "user should still have 500 tokens");
        assertEq(token.balanceOf(smartAccount), 200e18, "smart account should have 200 tokens");
        assertEq(token.balanceOf(recipient.addr), 300e18, "recipient should have 300 tokens");
    }

    function test_topUp_AllowedSenderCanCall() external {
        // Set up bundle config: token as bundle token, mockVault as allowed sender
        address mockVault = makeAddr("vault");
        address[] memory bundleTokens = new address[](1);
        bundleTokens[0] = address(token);

        address[] memory bundleSenders = new address[](1);
        bundleSenders[0] = mockVault;

        address smartAccount = factory.createAccount(user.addr, cca.addr, bundleTokens, bundleSenders, 0);

        // Pre-fund smart account with user's own funds (required by topUp check)
        token.mint(smartAccount, 500e18);

        // Mint tokens to mockVault and approve the smart account to pull them.
        // topUp calls executeFromExecutor, so the smart account is msg.sender in transferFrom.
        token.mint(mockVault, 1000e18);
        vm.prank(mockVault);
        token.approve(smartAccount, 1000e18);

        // Allowed sender (mockVault) can call topUp through the smart account
        vm.prank(mockVault);
        ITokenBundle(smartAccount).topUp(mockVault, address(token), 500e18);

        assertEq(token.balanceOf(smartAccount), 1000e18, "smart account should have 1000 tokens");
        assertEq(
            bundlePlugin.balanceOf(smartAccount, address(token)),
            1000e18,
            "smart account should have 1000 tokens in bundle"
        );

        // Disallowed sender (cca) must revert with InvalidCaller from CallerHook
        address hacker = makeAddr("hacker");
        vm.prank(hacker);
        vm.expectRevert(CallerHook.InvalidCaller.selector);
        ITokenBundle(smartAccount).topUp(mockVault, address(token), 1e18);
    }

    function test_BundleSpendProtector_BlocksTransferExceedingFreeBalance() external {
        address mockVault = makeAddr("vault");
        address[] memory bundleTokens = new address[](1);
        bundleTokens[0] = address(token);
        address[] memory bundleSenders = new address[](1);
        bundleSenders[0] = mockVault;

        address smartAccount = factory.createAccount(user.addr, cca.addr, bundleTokens, bundleSenders, 0);
        vm.deal(smartAccount, 0.1 ether);

        // Pre-fund SA with user's own funds for topUp + extra free balance
        token.mint(user.addr, 800e18);
        vm.prank(user.addr);
        require(token.transfer(smartAccount, 800e18), "user->SA transfer failed");

        // Bundle deposit: 600 tokens (locked) — SA already has 800 >= 600 ✓
        token.mint(mockVault, 600e18);
        vm.prank(mockVault);
        token.approve(smartAccount, 600e18);
        vm.prank(mockVault);
        ITokenBundle(smartAccount).topUp(mockVault, address(token), 600e18);

        // total=1400, bundle=1200, free=200
        assertEq(token.balanceOf(smartAccount), 1400e18);
        assertEq(bundlePlugin.balanceOf(smartAccount, address(token)), 1200e18);

        // Attempt 1: transfer 300 (exceeds free=200) → hook reverts, UserOp marked failed (success=false)
        // Note: ERC-4337 handleOps does NOT revert on execution failures; it emits UserOperationEvent(success=false)
        bytes memory callData300 = abi.encodePacked(
            Kernel.executeUserOp.selector,
            abi.encodeWithSelector(
                Kernel.execute.selector,
                _execMode(),
                ExecLib.encodeSingle(
                    address(token), 0, abi.encodeWithSelector(token.transfer.selector, recipient.addr, 300e18)
                )
            )
        );
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = _buildUserOp(smartAccount, callData300, user.privKey);
        entrypoint.handleOps(ops, payable(ENTRYPOINT_BENEFICIARY));

        // Transfer was blocked: recipient has no tokens, smart account still has 1400
        assertEq(token.balanceOf(recipient.addr), 0, "recipient should have 0 tokens after blocked transfer");
        assertEq(token.balanceOf(smartAccount), 1400e18, "SA should still have 1400 tokens after blocked transfer");

        // Attempt 2: transfer 150 (within free=200) → success
        bytes memory callData150 = abi.encodePacked(
            Kernel.executeUserOp.selector,
            abi.encodeWithSelector(
                Kernel.execute.selector,
                _execMode(),
                ExecLib.encodeSingle(
                    address(token), 0, abi.encodeWithSelector(token.transfer.selector, recipient.addr, 150e18)
                )
            )
        );
        ops[0] = _buildUserOp(smartAccount, callData150, user.privKey);
        entrypoint.handleOps(ops, payable(ENTRYPOINT_BENEFICIARY));

        assertEq(token.balanceOf(recipient.addr), 150e18, "recipient should have 150 tokens");
        assertEq(token.balanceOf(smartAccount), 1250e18, "SA should have 1250 tokens");
    }
}
