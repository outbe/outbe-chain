// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;
import {BaseAATest} from "./BaseAATest.sol";
import {ITokenBundle} from "src/interfaces/ITokenBundle.sol";
import {BundleModulePlugin} from "src/BundleModulePlugin.sol";
import {MockUSD} from "src/mocks/MockUSD.sol";
import {MockFeeToken} from "src/mocks/MockFeeToken.sol";
import {ICca} from "@precompiles/ICca.sol";

contract CCAFlow is BaseAATest {
    function test_PaymentAndMatchingRelease() public {
        address a = _account();
        _open(a);
        _fund(a, 100);
        assertEq(token.balanceOf(a), 0);
        assertEq(token.balanceOf(address(bundlePlugin)), 200);
        assertTrue(_pay(a, 40));
        assertEq(token.balanceOf(recipient), 40);
        assertEq(token.balanceOf(a), 40);
        assertEq(bundlePlugin.balanceOf(a, address(token)), 120);
    }

    function test_InsufficientMatchingFundsRollsBackSenderTransfer() public {
        address a = _account();
        _open(a);
        token.mint(vault, 100);
        vm.startPrank(vault);
        token.approve(address(bundlePlugin), 100);
        vm.expectRevert();
        bundlePlugin.topUpFor(a, address(token), 100);
        vm.stopPrank();
        assertEq(token.balanceOf(vault), 100);
        assertEq(bundlePlugin.balanceOf(a, address(token)), 0);
    }

    function test_UnopenedAndClosedCannotFund() public {
        address a = _account();
        token.mint(vault, 10);
        vm.prank(vault);
        vm.expectRevert();
        bundlePlugin.topUpFor(a, address(token), 10);
        _open(a);
        assertTrue(_ownerCall(a, a, abi.encodeWithSignature("closeBundle()")));
        vm.prank(vault);
        vm.expectRevert();
        bundlePlugin.topUpFor(a, address(token), 10);
    }

    function test_DailyLimitAndFailedExecutionDoNotDebit() public {
        address a = _account();
        _open(a);
        _fund(a, 2000e6);
        assertTrue(_pay(a, 1000e6));
        uint256 nonce = bundlePlugin.paymentNonce(a);
        assertFalse(_pay(a, 1));
        assertEq(bundlePlugin.paymentNonce(a), nonce);
        vm.warp(block.timestamp + 1 days);
        assertTrue(_pay(a, 1));
    }

    function test_ReserveCeiling() public {
        address a = _account();
        _open(a);
        _fund(a, 10);
        assertFalse(_pay(a, 11));
        assertEq(bundlePlugin.balanceOf(a, address(token)), 20);
        assertEq(bundlePlugin.paymentNonce(a), 0);
    }

    function test_OwnerCannotForgePaymentOrReplayAuthorization() public {
        address a = _account();
        _open(a);
        _fund(a, 100);
        ITokenBundle.Payment memory p = _payment(a, 10);
        bytes memory bad = abi.encodeCall(ITokenBundle.spend, (p, _sign(bundlePlugin.paymentDigest(p), userKey)));
        assertFalse(_ownerCall(a, address(bundlePlugin), bad));
        bytes memory valid = abi.encodeCall(ITokenBundle.spend, (p, _sign(bundlePlugin.paymentDigest(p), ccaKey)));
        assertTrue(_ownerCall(a, address(bundlePlugin), valid));
        assertFalse(_ownerCall(a, address(bundlePlugin), valid));
    }

    function test_CcaCannotExecuteOrdinaryTokenTransfer() public {
        address a = _account();
        _open(a);
        token.mint(a, 10);
        assertFalse(
            _submit(
                _op(
                    a,
                    _executeData(address(token), abi.encodeCall(token.transfer, (recipient, 10))),
                    factory.ccaPermission(address(token)),
                    ccaKey
                )
            )
        );
        assertEq(token.balanceOf(a), 10);
    }

    function test_SuspendedCcaCannotSpend() public {
        address a = _account();
        _open(a);
        _fund(a, 10);
        registry.setState(cca, ICca.State.Suspended);
        assertFalse(_pay(a, 1));
        assertEq(bundlePlugin.balanceOf(a, address(token)), 20);
    }

    function test_ExistingAllowanceCannotDrainCustody() public {
        address a = _account();
        assertTrue(_ownerCall(a, address(token), abi.encodeCall(token.approve, (recipient, type(uint256).max))));
        _open(a);
        _fund(a, 10);
        vm.prank(recipient);
        vm.expectRevert();
        token.transferFrom(a, recipient, 1);
        assertEq(token.balanceOf(address(bundlePlugin)), 20);
    }

    function test_PaymentSignatureBindsAccountTokenRecipientAndChain() public {
        address a = _account();
        _open(a);
        _fund(a, 100);
        ITokenBundle.Payment memory p = _payment(a, 10);
        bytes memory signature = _sign(bundlePlugin.paymentDigest(p), ccaKey);
        p.recipient = user;
        assertFalse(_ownerCall(a, address(bundlePlugin), abi.encodeCall(ITokenBundle.spend, (p, signature))));
        p.recipient = recipient;
        p.token = address(1);
        assertFalse(_ownerCall(a, address(bundlePlugin), abi.encodeCall(ITokenBundle.spend, (p, signature))));
        p.token = address(token);
        p.account = user;
        assertFalse(_ownerCall(a, address(bundlePlugin), abi.encodeCall(ITokenBundle.spend, (p, signature))));
        p.account = a;
        uint256 chain = block.chainid;
        vm.chainId(chain + 1);
        assertFalse(_ownerCall(a, address(bundlePlugin), abi.encodeCall(ITokenBundle.spend, (p, signature))));
        vm.chainId(chain);
        vm.warp(p.deadline + 1);
        assertFalse(_ownerCall(a, address(bundlePlugin), abi.encodeCall(ITokenBundle.spend, (p, signature))));
        assertEq(bundlePlugin.paymentNonce(a), 0);
        assertEq(bundlePlugin.balanceOf(a, address(token)), 200);
    }

    function test_FeeTokenDepositRevertsWithoutTakingEitherContribution() public {
        address a = _account();
        MockFeeToken fee = new MockFeeToken(100);
        address[] memory ts = new address[](1);
        ts[0] = address(fee);
        _openTokens(a, ts);
        fee.mint(a, 10000);
        fee.mint(vault, 10000);
        assertTrue(_ownerCall(a, address(fee), abi.encodeCall(fee.approve, (address(bundlePlugin), 10000))));
        vm.startPrank(vault);
        fee.approve(address(bundlePlugin), 10000);
        vm.expectRevert(BundleModulePlugin.NonExactTransfer.selector);
        bundlePlugin.topUpFor(a, address(fee), 10000);
        vm.stopPrank();
        assertEq(fee.balanceOf(a), 10000);
        assertEq(fee.balanceOf(vault), 10000);
        assertEq(fee.balanceOf(address(bundlePlugin)), 0);
    }

    function test_RecipientCanBeAccount() public {
        address a = _account();
        _open(a);
        _fund(a, 10);
        ITokenBundle.Payment memory p = _payment(a, 10);
        p.recipient = a;
        assertTrue(
            _ownerCall(
                a,
                address(bundlePlugin),
                abi.encodeCall(ITokenBundle.spend, (p, _sign(bundlePlugin.paymentDigest(p), ccaKey)))
            )
        );
        assertEq(token.balanceOf(a), 20);
        assertEq(bundlePlugin.balanceOf(a, address(token)), 0);
    }

    function test_FalseReturnPaymentRollsBackReserveNonceAndTransfers() public {
        CallbackToken callback = new CallbackToken(bundlePlugin);
        token = callback;
        address a = _account();
        _open(a);
        _fund(a, 10);
        callback.setFalseReturn(true);
        assertFalse(_pay(a, 10));
        assertEq(bundlePlugin.balanceOf(a, address(token)), 20);
        assertEq(bundlePlugin.paymentNonce(a), 0);
        assertEq(token.balanceOf(recipient), 0);
        assertEq(token.balanceOf(a), 0);
        callback.setFalseReturn(false);
        assertTrue(_pay(a, 10));
    }

    function test_TokenCallbackCannotReenterCustody() public {
        CallbackToken callback = new CallbackToken(bundlePlugin);
        token = callback;
        address a = _account();
        _open(a);
        _fund(a, 10);
        assertTrue(_pay(a, 10));
        assertEq(uint8(bundlePlugin.status(address(callback))), 0);
        assertEq(bundlePlugin.balanceOf(a, address(token)), 0);
    }

    function testFuzz_PaymentConservesCustody(uint96 input) public {
        uint256 amount = bound(input, 1, 1000e6);
        address a = _account();
        _open(a);
        _fund(a, amount);
        assertTrue(_pay(a, amount));
        assertEq(token.balanceOf(address(bundlePlugin)), 0);
        assertEq(token.balanceOf(a), amount);
        assertEq(token.balanceOf(recipient), amount);
    }
}

contract CallbackToken is MockUSD {
    BundleModulePlugin private immutable custody;
    bool private falseReturn;

    constructor(BundleModulePlugin custody_) {
        custody = custody_;
    }

    function setFalseReturn(bool value) external {
        falseReturn = value;
    }

    function transfer(address to, uint256 amount) public override returns (bool) {
        super.transfer(to, amount);
        return !falseReturn;
    }

    function _update(address from, address to, uint256 amount) internal override {
        if (from != address(0)) {
            (bool success,) = address(custody).call(abi.encodeCall(BundleModulePlugin.retire, ()));
            require(!success, "custody reentrancy succeeded");
        }
        super._update(from, to, amount);
    }
}
