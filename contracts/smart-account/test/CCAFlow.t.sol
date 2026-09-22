// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
import {BaseAATest} from "./BaseAATest.sol";
import {ICcaRegistry} from "@precompiles/ICcaRegistry.sol";
import {SmartAccountFactory} from "src/SmartAccountFactory.sol";
import {MockUSD} from "src/mocks/MockUSD.sol";
import {WithdrawalLimitPolicy} from "src/WithdrawalLimitPolicy.sol";
import {Kernel} from "@zerodev/kernel/Kernel.sol";
import {IEntryPoint} from "account-abstraction/interfaces/IEntryPoint.sol";
import {PackedUserOperation} from "account-abstraction/interfaces/PackedUserOperation.sol";
import {PermissionId} from "@zerodev/kernel/types/Types.sol";

contract CCAFlow is BaseAATest {
    function test_CCA_BlockedByDailyLimit() external {
        address smartAccount = _deployAccount();
        vm.deal(smartAccount, 0.1 ether);
        _fund(smartAccount, 2000e6);

        // 1001e6 exceeds 1000e6 daily limit -> policy reverts with custom error
        PackedUserOperation memory op = _buildCcaUserOp(smartAccount, recipient.addr, 1001e6);
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = op;

        vm.expectRevert(
            abi.encodeWithSelector(
                IEntryPoint.FailedOpWithRevert.selector,
                0,
                "AA23 reverted",
                abi.encodeWithSelector(WithdrawalLimitPolicy.WithdrawalLimitExceeded.selector, 1001e6, 1000e6)
            )
        );
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
    }

    function test_CCA_CumulativeLimit() external {
        address smartAccount = _deployAccount();
        vm.deal(smartAccount, 0.1 ether);
        _fund(smartAccount, 2000e6);

        // First withdrawal: 600e6 -> succeeds
        _ccaWithdraw(smartAccount, recipient.addr, 600e6);
        assertEq(token.balanceOf(recipient.addr), 600e6);

        // Second withdrawal: 500e6 -> cumulative 1100e6 exceeds 1000e6 -> fails
        PackedUserOperation memory op = _buildCcaUserOp(smartAccount, recipient.addr, 500e6);
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = op;

        vm.expectRevert(
            abi.encodeWithSelector(
                IEntryPoint.FailedOpWithRevert.selector,
                0,
                "AA23 reverted",
                abi.encodeWithSelector(WithdrawalLimitPolicy.WithdrawalLimitExceeded.selector, 1100e6, 1000e6)
            )
        );
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));

        // Balance unchanged after failed op
        assertEq(token.balanceOf(recipient.addr), 600e6, "recipient balance should still be 600");
    }

    function test_CCA_WindowResets() external {
        address smartAccount = _deployAccount();
        vm.deal(smartAccount, 0.1 ether);
        _fund(smartAccount, 3000e6);

        // Use 1000e6 in first window
        _ccaWithdraw(smartAccount, recipient.addr, 1000e6);
        assertEq(token.balanceOf(recipient.addr), 1000e6);

        // Warp past the window
        vm.warp(block.timestamp + 1 days + 1);

        // New window: withdraw another 800e6 -> succeeds
        _ccaWithdraw(smartAccount, recipient.addr, 800e6);
        assertEq(token.balanceOf(recipient.addr), 1800e6, "recipient should have 1800 total");
    }

    function test_CCA_CannotTransferOtherToken() external {
        address smartAccount = _deployAccount();
        vm.deal(smartAccount, 0.1 ether);

        MockUSD otherToken = new MockUSD();
        otherToken.mint(smartAccount, 500e6);

        // Build UserOp targeting otherToken - outside this permission -> policy rejects
        bytes32 execMode = _execMode();
        bytes memory transferCall = abi.encodeWithSelector(otherToken.transfer.selector, recipient.addr, uint256(100e6));
        bytes memory innerExecute =
            abi.encodeWithSelector(Kernel.execute.selector, execMode, _single(address(otherToken), 0, transferCall));
        bytes memory callData = innerExecute;

        PackedUserOperation memory op = _buildCcaUserOpRaw(smartAccount, callData, address(token));
        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = op;

        vm.expectRevert();
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
        // The UserOp fails (success=false from EntryPointEvent) - recipient gets nothing
        assertEq(otherToken.balanceOf(recipient.addr), 0, "recipient should have no other token");
    }

    function test_NonCCA_SignatureRejected() external {
        address smartAccount = _deployAccount();
        vm.deal(smartAccount, 0.1 ether);
        _fund(smartAccount, 500e6);

        // Build a UserOp but sign with user key instead of cca key (permission signature format)
        PackedUserOperation memory op = _buildCcaUserOp(smartAccount, recipient.addr, 100e6);
        // Overwrite the signer slice with the wrong key -> ECDSASigner recovers != cca -> AA24.
        op.signature = _permSignature(entrypoint.getUserOpHash(op), user.privKey);

        PackedUserOperation[] memory ops = new PackedUserOperation[](1);
        ops[0] = op;

        vm.expectRevert(abi.encodeWithSelector(IEntryPoint.FailedOp.selector, 0, "AA24 signature error"));
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
    }

    function test_CCA_SeparatePermissionsPerToken() external {
        MockUSD token2 = new MockUSD();

        address[] memory withdrawalTokens = new address[](2);
        withdrawalTokens[0] = address(token);
        withdrawalTokens[1] = address(token2);

        address smartAccount = factory.createAccount(user.addr, cca.addr, withdrawalTokens, 0);
        vm.deal(smartAccount, 0.1 ether);

        // Pre-fund SA with user's own funds for each token
        token.mint(smartAccount, 2000e6);
        token2.mint(smartAccount, 2000e6);
        // Withdraw 800e6 of token via CCA permission for token
        _ccaWithdrawToken(smartAccount, recipient.addr, 800e6, address(token));
        assertEq(token.balanceOf(recipient.addr), 800e6);

        // Withdraw 900e6 of token2 via CCA permission for token2 - separate limit
        _ccaWithdrawToken(smartAccount, recipient.addr, 900e6, address(token2));
        assertEq(token2.balanceOf(recipient.addr), 900e6);
    }

    function test_W04_RevertingExec_StillDebits() external {
        address smartAccount = _deployAccount();
        vm.deal(smartAccount, 0.1 ether);
        // The account holds no tokens, so the transfer reverts on insufficient balance.
        uint256 amount = 500e6; // <= DAILY_LIMIT (1000e6): validation passes; > 0 balance: execution reverts.

        _ccaWithdraw(smartAccount, recipient.addr, amount);

        // Execution reverted (recipient received nothing) but the validation-phase debit persists.
        assertEq(token.balanceOf(recipient.addr), 0, "transfer must have reverted");
        bytes32 permId = bytes32(PermissionId.unwrap(_ccaPermId(address(token))));
        (uint256 used,) = withdrawalLimitPolicy.states(permId, smartAccount);
        assertEq(used, amount, "usedAmount is debited in validation and survives the reverted execution");
    }

    function testFuzz_CCA_WithinLimit(uint96 amount) external {
        amount = uint96(bound(amount, 1, 1000e6));

        address smartAccount = _deployAccount();
        vm.deal(smartAccount, 0.1 ether);
        _fund(smartAccount, 1000e6);

        _ccaWithdraw(smartAccount, recipient.addr, amount);
        assertEq(token.balanceOf(recipient.addr), amount, "recipient should receive exact amount");
        assertEq(token.balanceOf(smartAccount), 1000e6 - amount);
    }

    function test_CreateAccount_SucceedsWhenCcaActive() external {
        address[] memory withdrawalTokens = _tokens();
        ccaRegistry.setState(cca.addr, ICcaRegistry.State.Active);

        address account = factory.createAccount(user.addr, cca.addr, withdrawalTokens, 7);
        assertTrue(account.code.length > 0, "account should be deployed");
    }

    function test_RevertWhen_CcaDeregistering() external {
        address[] memory withdrawalTokens = _tokens();
        ccaRegistry.setState(cca.addr, ICcaRegistry.State.Deregistering);

        vm.expectRevert(
            abi.encodeWithSelector(ICcaRegistry.CcaNotActive.selector, cca.addr, ICcaRegistry.State.Deregistering)
        );
        factory.createAccount(user.addr, cca.addr, withdrawalTokens, 8);
    }

    function test_RevertWhen_CcaDeregistered() external {
        address[] memory withdrawalTokens = _tokens();
        ccaRegistry.setState(cca.addr, ICcaRegistry.State.Deregistered);

        vm.expectRevert(
            abi.encodeWithSelector(ICcaRegistry.CcaNotActive.selector, cca.addr, ICcaRegistry.State.Deregistered)
        );
        factory.createAccount(user.addr, cca.addr, withdrawalTokens, 9);
    }

    function test_RevertWhen_CcaNeverRegistered() external {
        address[] memory withdrawalTokens = _tokens();
        (address stranger,) = makeAddrAndKey("unregistered-cca");

        vm.expectRevert(bytes("CCA is not registered"));
        factory.createAccount(user.addr, stranger, withdrawalTokens, 10);
    }

    function test_RevertWhen_CcaBonding() external {
        address[] memory withdrawalTokens = _tokens();
        address bondingCca = makeAddr("bonding-cca");
        vm.deal(bondingCca, 1);
        vm.prank(bondingCca);
        ccaRegistry.bond{value: 1}("Test CCA");
        vm.expectRevert(
            abi.encodeWithSelector(ICcaRegistry.CcaNotActive.selector, bondingCca, ICcaRegistry.State.Bonding)
        );
        factory.createAccount(user.addr, bondingCca, withdrawalTokens, 10);
    }

    function test_GetAccountAddress_IgnoresCcaState() external {
        address[] memory withdrawalTokens = _tokens();

        ccaRegistry.setState(cca.addr, ICcaRegistry.State.Active);
        address whenActive = factory.getAccountAddress(user.addr, cca.addr, withdrawalTokens, 11);

        ccaRegistry.setState(cca.addr, ICcaRegistry.State.Deregistered);
        address whenDeregistered = factory.getAccountAddress(user.addr, cca.addr, withdrawalTokens, 11);

        assertEq(whenActive, whenDeregistered, "prediction must not depend on registry state");
    }

    function _tokens() private view returns (address[] memory withdrawalTokens) {
        withdrawalTokens = new address[](1);
        withdrawalTokens[0] = address(token);
    }

    function test_CCA_SameBundleCannotExceedCap() public {
        address account = _deployAccount();
        vm.deal(account, 1 ether);
        _fund(account, 2000e6);
        PackedUserOperation[] memory ops = new PackedUserOperation[](2);
        ops[0] = _buildCcaUserOp(account, recipient.addr, 600e6);
        ops[1] = _buildCcaUserOp(account, recipient.addr, 600e6);
        ops[1].nonce++;
        ops[1].signature = _permSignature(entrypoint.getUserOpHash(ops[1]), cca.privKey);
        vm.expectRevert();
        _bundle(ops, payable(ENTRYPOINT_BENEFICIARY));
        assertEq(token.balanceOf(recipient.addr), 0);
    }
}
