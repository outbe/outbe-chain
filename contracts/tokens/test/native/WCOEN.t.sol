// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {WCOEN} from "../../src/native/WCOEN.sol";

contract WCOENTest is Test {
    WCOEN internal token;

    function setUp() public {
        token = new WCOEN();
    }

    function test_DepositAndWithdraw_UpdateSupply() public {
        address alice = makeAddr("alice");

        vm.deal(alice, 2 ether);
        vm.prank(alice);
        token.deposit{value: 2 ether}();

        assertEq(token.balanceOf(alice), 2 ether);
        assertEq(token.totalSupply(), 2 ether);
        assertEq(address(token).balance, 2 ether);

        vm.prank(alice);
        token.withdraw(1 ether);

        assertEq(token.balanceOf(alice), 1 ether);
        assertEq(token.totalSupply(), 1 ether);
        assertEq(address(token).balance, 1 ether);
    }

    function test_Receive_DepositsNativeCoin() public {
        address alice = makeAddr("alice");

        vm.deal(alice, 1 ether);
        vm.prank(alice);
        (bool success,) = address(token).call{value: 1 ether}("");

        assertTrue(success);
        assertEq(token.balanceOf(alice), 1 ether);
        assertEq(token.totalSupply(), 1 ether);
        assertEq(address(token).balance, 1 ether);
    }

    function testFuzz_DepositAndWithdraw_RestoresSupply(address account, uint256 amount) public {
        vm.assume(account != address(0));
        vm.assume(account != address(token));
        vm.assume(uint160(account) > 0xffff);
        vm.assume(account.code.length == 0);
        vm.assume(amount <= type(uint128).max);

        vm.deal(account, amount);
        vm.startPrank(account);
        token.deposit{value: amount}();
        token.withdraw(amount);
        vm.stopPrank();

        assertEq(token.balanceOf(account), 0);
        assertEq(token.totalSupply(), 0);
        assertEq(address(token).balance, 0);
    }
}
