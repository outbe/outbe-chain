// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {MockSmartAccount} from "../src/MockSmartAccount.sol";

contract TransferToken {
    mapping(address => uint256) public balanceOf;

    constructor(address holder) {
        balanceOf[holder] = 10_000;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}

contract MockSmartAccountTest is Test {
    address constant USER = address(0xA11CE);
    address constant CCA = address(0xCCA);
    MockSmartAccount account;
    TransferToken token;

    function setUp() public {
        account = new MockSmartAccount(USER, CCA);
        token = new TransferToken(address(account));
        vm.deal(address(account), 2 ether);
    }

    function testBothActorsCanTransferTokensAndNativeValue() public {
        address[2] memory actors = [USER, CCA];
        for (uint256 i; i < actors.length; ++i) {
            vm.prank(actors[i]);
            bytes memory result = account.execute(
                address(token), 0, abi.encodeWithSignature("transfer(address,uint256)", actors[i], 1000)
            );
            assertTrue(abi.decode(result, (bool)));
            assertEq(token.balanceOf(actors[i]), 1000);
            vm.prank(actors[i]);
            account.execute(actors[i], 1 ether, "");
            assertEq(actors[i].balance, 1 ether);
        }
    }

    function testOutsiderCannotExecute() public {
        vm.expectRevert("UNAUTHORIZED");
        account.execute(address(token), 0, abi.encodeWithSignature("transfer(address,uint256)", address(this), 1000));
        assertEq(token.balanceOf(address(account)), 10_000);
    }

    function testTargetRevertBubblesAndRollsBackValue() public {
        vm.prank(USER);
        vm.expectRevert("TARGET_REVERT");
        account.execute(address(this), 1 ether, abi.encodeCall(this.revertingTarget, ()));
        assertEq(address(account).balance, 2 ether);
    }

    function revertingTarget() external payable {
        revert("TARGET_REVERT");
    }
}
