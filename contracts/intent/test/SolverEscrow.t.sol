// SPDX-License-Identifier: MIT
pragma solidity ^0.8.25;

import {Test} from "forge-std/Test.sol";
import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {MockERC20} from "./mocks/MockERC20.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";
import {Scope} from "the-compact/src/types/Scope.sol";

import {SolverAllocator} from "../src/allocators/SolverAllocator.sol";
import {ISolverEscrow} from "../src/interfaces/ISolverEscrow.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";

import {MockTheCompact} from "./mocks/MockTheCompact.sol";

contract SolverEscrowTest is Test {
    ERC20 internal token;
    MockTheCompact internal compact;
    SolverAllocator internal allocator;
    SolverEscrow internal escrow;

    address internal solver;
    address internal authorizedCaller;
    bytes12 internal lockTag;
    uint256 internal constant COLLATERAL_BPS = 1000; // 10%

    event Deposited(address indexed solver, address indexed token, uint256 amount);
    event Withdrawn(address indexed solver, address indexed token, uint256 amount);
    event CollateralSlashed(bytes32 indexed orderId, address indexed solver, address token, uint256 amount);
    event CollateralBpsUpdated(uint256 oldBps, uint256 newBps);
    event RewardDistributed(address indexed receiver, address indexed token, uint256 amount);

    function setUp() public {
        solver = makeAddr("solver");
        authorizedCaller = makeAddr("authorizedCaller");
        token = new MockERC20("Test Token", "TT");

        // Deploy stack: Compact → Allocator → Escrow → wire arbiter
        compact = new MockTheCompact();
        allocator = new SolverAllocator(address(compact));
        lockTag = allocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.TenMinutes);
        escrow = new SolverEscrow(address(compact), lockTag, COLLATERAL_BPS);
        escrow.setAuthorizedCaller(authorizedCaller);
        allocator.setArbiter(address(escrow));

        // Solver approves escrow as ERC6909 operator (required for transferFrom)
        vm.prank(solver);
        compact.setOperator(address(escrow), true);

        // Fund solver
        deal(address(token), solver, 1_000_000);
        deal(solver, 1_000_000);
    }

    // ============ deposit ERC20 ============

    function test_deposit_ERC20_works() public {
        uint256 amount = 500;

        vm.startPrank(solver);
        token.approve(address(escrow), amount);

        vm.expectEmit(true, true, false, true);
        emit Deposited(solver, address(token), amount);

        escrow.deposit(address(token), amount);
        vm.stopPrank();

        uint256 id = escrow.lockId(address(token));
        assertEq(compact.balanceOf(solver, id), amount, "ERC6909 balance should match deposit");
    }

    function test_deposit_ERC20_multipleDeposits() public {
        vm.startPrank(solver);
        token.approve(address(escrow), 1000);

        vm.expectEmit(true, true, false, true);
        emit Deposited(solver, address(token), 300);
        escrow.deposit(address(token), 300);

        vm.expectEmit(true, true, false, true);
        emit Deposited(solver, address(token), 200);
        escrow.deposit(address(token), 200);

        vm.stopPrank();

        uint256 id = escrow.lockId(address(token));
        assertEq(compact.balanceOf(solver, id), 500, "cumulative ERC6909 balance");
    }

    function test_deposit_ERC20_zeroAmount_reverts() public {
        vm.prank(solver);
        vm.expectRevert(SolverEscrow.InvalidAmount.selector);
        escrow.deposit(address(token), 0);
    }

    function test_deposit_withoutOperator_reverts() public {
        address noOperatorSolver = makeAddr("noOperatorSolver");
        deal(address(token), noOperatorSolver, 1000);

        vm.prank(noOperatorSolver);
        vm.expectRevert(SolverEscrow.OperatorNotApproved.selector);
        escrow.deposit(address(token), 100);
    }

    // ============ deposit native ============

    function test_deposit_native_works() public {
        uint256 amount = 1000;

        vm.startPrank(solver);

        vm.expectEmit(true, true, false, true);
        emit Deposited(solver, address(0), amount);

        escrow.deposit{value: amount}(address(0), 0);
        vm.stopPrank();

        uint256 id = escrow.lockId(address(0));
        assertEq(compact.balanceOf(solver, id), amount, "ERC6909 native balance");
    }

    function test_deposit_native_zeroValue_reverts() public {
        vm.prank(solver);
        vm.expectRevert(SolverEscrow.InvalidAmount.selector);
        escrow.deposit{value: 0}(address(0), 0);
    }

    // ============ withdraw ERC20 ============

    function test_withdraw_ERC20_works() public {
        uint256 amount = 500;

        vm.startPrank(solver);
        token.approve(address(escrow), amount);
        escrow.deposit(address(token), amount);

        uint256 balanceBefore = token.balanceOf(solver);

        vm.expectEmit(true, true, false, true);
        emit Withdrawn(solver, address(token), amount);

        escrow.withdraw(address(token), amount);
        vm.stopPrank();

        assertEq(token.balanceOf(solver) - balanceBefore, amount, "tokens returned to solver");
        assertEq(compact.balanceOf(solver, escrow.lockId(address(token))), 0, "ERC6909 burned");
    }

    function test_withdraw_ERC20_zeroMeansAll() public {
        uint256 amount = 500;

        vm.startPrank(solver);
        token.approve(address(escrow), amount);
        escrow.deposit(address(token), amount);

        escrow.withdraw(address(token), 0);
        vm.stopPrank();

        assertEq(compact.balanceOf(solver, escrow.lockId(address(token))), 0, "all withdrawn");
    }

    // ============ withdraw native ============

    function test_withdraw_native_works() public {
        uint256 amount = 1000;

        vm.startPrank(solver);
        escrow.deposit{value: amount}(address(0), 0);

        uint256 balanceBefore = solver.balance;

        vm.expectEmit(true, true, false, true);
        emit Withdrawn(solver, address(0), amount);

        escrow.withdraw(address(0), amount);
        vm.stopPrank();

        assertEq(solver.balance - balanceBefore, amount, "ETH returned to solver");
        assertEq(compact.balanceOf(solver, escrow.lockId(address(0))), 0, "ERC6909 burned");
    }

    // ============ withdraw insufficient ============

    function test_withdraw_insufficientBalance_reverts() public {
        vm.startPrank(solver);
        token.approve(address(escrow), 100);
        escrow.deposit(address(token), 100);

        vm.expectRevert(ISolverEscrow.InsufficientAvailableBalance.selector);
        escrow.withdraw(address(token), 200);
        vm.stopPrank();
    }

    // ============ lockCollateral ============

    function test_lockCollateral_works() public {
        uint256 amount = 500;
        bytes32 orderId = keccak256("order1");

        // Deposit first
        vm.startPrank(solver);
        token.approve(address(escrow), amount);
        escrow.deposit(address(token), amount);
        vm.stopPrank();

        // Lock
        vm.prank(authorizedCaller);
        escrow.lockCollateral(orderId, solver, address(token), 100);

        // Check state
        (address lockSolver, address lockToken, uint256 lockAmount) = escrow.locks(orderId);
        assertEq(lockSolver, solver);
        assertEq(lockToken, address(token));
        assertEq(lockAmount, 100);

        uint256 id = escrow.lockId(address(token));
        assertEq(escrow.totalLocked(solver, id), 100);
    }

    function test_lockCollateral_onlyAuthorizedCaller_reverts() public {
        vm.prank(solver);
        vm.expectRevert(SolverEscrow.UnauthorizedCaller.selector);
        escrow.lockCollateral(keccak256("order1"), solver, address(token), 100);
    }

    function test_lockCollateral_insufficientBalance_reverts() public {
        vm.prank(authorizedCaller);
        vm.expectRevert(ISolverEscrow.InsufficientAvailableBalance.selector);
        escrow.lockCollateral(keccak256("order1"), solver, address(token), 100);
    }

    function test_lockCollateral_duplicateOrder_reverts() public {
        bytes32 orderId = keccak256("order1");

        vm.startPrank(solver);
        token.approve(address(escrow), 500);
        escrow.deposit(address(token), 500);
        vm.stopPrank();

        vm.startPrank(authorizedCaller);
        escrow.lockCollateral(orderId, solver, address(token), 100);

        vm.expectRevert(ISolverEscrow.LockAlreadyExists.selector);
        escrow.lockCollateral(orderId, solver, address(token), 100);
        vm.stopPrank();
    }

    // ============ unlockCollateral ============

    function test_unlockCollateral_works() public {
        bytes32 orderId = keccak256("order1");

        vm.startPrank(solver);
        token.approve(address(escrow), 500);
        escrow.deposit(address(token), 500);
        vm.stopPrank();

        vm.startPrank(authorizedCaller);
        escrow.lockCollateral(orderId, solver, address(token), 100);

        escrow.unlockCollateral(orderId);
        vm.stopPrank();

        // Lock removed, totalLocked decremented
        (,, uint256 lockAmount) = escrow.locks(orderId);
        assertEq(lockAmount, 0);
        assertEq(escrow.totalLocked(solver, escrow.lockId(address(token))), 0);
    }

    function test_unlockCollateral_notFound_reverts() public {
        vm.prank(authorizedCaller);
        vm.expectRevert(ISolverEscrow.LockNotFound.selector);
        escrow.unlockCollateral(keccak256("nonexistent"));
    }

    // ============ slashCollateral ============

    function test_slashCollateral_works() public {
        bytes32 orderId = keccak256("order1");
        uint256 depositAmount = 500;
        uint256 lockAmount = 100;

        vm.startPrank(solver);
        token.approve(address(escrow), depositAmount);
        escrow.deposit(address(token), depositAmount);
        vm.stopPrank();

        vm.startPrank(authorizedCaller);
        escrow.lockCollateral(orderId, solver, address(token), lockAmount);

        vm.expectEmit(true, true, false, true);
        emit CollateralSlashed(orderId, solver, address(token), lockAmount);
        escrow.slashCollateral(orderId);
        vm.stopPrank();

        // Lock removed, totalLocked decremented
        (,, uint256 amt) = escrow.locks(orderId);
        assertEq(amt, 0);
        assertEq(escrow.totalLocked(solver, escrow.lockId(address(token))), 0);

        // ERC6909 moved from solver to escrow
        uint256 id = escrow.lockId(address(token));
        assertEq(compact.balanceOf(solver, id), depositAmount - lockAmount, "solver balance reduced");
        assertEq(compact.balanceOf(address(escrow), id), lockAmount, "escrow received slashed ERC6909");
    }

    // ============ withdraw with locked funds ============

    function test_withdraw_lockedFunds_partialAvailable() public {
        vm.startPrank(solver);
        token.approve(address(escrow), 500);
        escrow.deposit(address(token), 500);
        vm.stopPrank();

        // Lock 200 of 500
        vm.prank(authorizedCaller);
        escrow.lockCollateral(keccak256("order1"), solver, address(token), 200);

        // Can withdraw up to 300
        vm.startPrank(solver);
        escrow.withdraw(address(token), 300);

        // Can't withdraw more
        vm.expectRevert(ISolverEscrow.InsufficientAvailableBalance.selector);
        escrow.withdraw(address(token), 1);
        vm.stopPrank();
    }

    // ============ hasMinCollateral with locks ============

    function test_hasMinCollateral_accountsForLocked() public {
        // Deposit 100, lock 50 → available = 50
        vm.startPrank(solver);
        token.approve(address(escrow), 100);
        escrow.deposit(address(token), 100);
        vm.stopPrank();

        vm.prank(authorizedCaller);
        escrow.lockCollateral(keccak256("order1"), solver, address(token), 50);

        // 10% of 500 = 50 → available 50 >= 50 → true
        assertTrue(escrow.hasMinCollateral(solver, address(token), 500));
        // 10% of 501 = 50.1 → rounds to 50 → true (integer division)
        assertTrue(escrow.hasMinCollateral(solver, address(token), 501));
        // 10% of 510 = 51 → available 50 < 51 → false
        assertFalse(escrow.hasMinCollateral(solver, address(token), 510));
    }

    function test_hasMinCollateral_sufficient() public {
        vm.startPrank(solver);
        token.approve(address(escrow), 100);
        escrow.deposit(address(token), 100);
        vm.stopPrank();

        assertTrue(escrow.hasMinCollateral(solver, address(token), 1000), "100 >= 10% of 1000");
    }

    function test_hasMinCollateral_insufficient() public {
        vm.startPrank(solver);
        token.approve(address(escrow), 99);
        escrow.deposit(address(token), 99);
        vm.stopPrank();

        assertFalse(escrow.hasMinCollateral(solver, address(token), 1000), "99 < 10% of 1000");
    }

    function test_hasMinCollateral_noDeposit() public view {
        assertFalse(escrow.hasMinCollateral(solver, address(token), 1000), "no deposit = 0 balance");
    }

    // ============ getBalance / getBalances ============

    function test_getBalance_works() public {
        vm.startPrank(solver);
        token.approve(address(escrow), 500);
        escrow.deposit(address(token), 500);
        vm.stopPrank();

        vm.prank(authorizedCaller);
        escrow.lockCollateral(keccak256("order1"), solver, address(token), 200);

        (uint256 total, uint256 locked, uint256 available) = escrow.getBalance(solver, address(token));
        assertEq(total, 500);
        assertEq(locked, 200);
        assertEq(available, 300);
    }

    function test_getBalances_works() public {
        vm.startPrank(solver);
        token.approve(address(escrow), 500);
        escrow.deposit(address(token), 500);
        vm.stopPrank();

        address[] memory tokens = new address[](2);
        tokens[0] = address(token);
        tokens[1] = address(0);

        ISolverEscrow.BalanceInfo[] memory infos = escrow.getBalances(solver, tokens);
        assertEq(infos.length, 2);
        assertEq(infos[0].total, 500);
        assertEq(infos[0].available, 500);
        assertEq(infos[1].total, 0);
    }

    // ============ setCollateralBps ============

    function test_setCollateralBps_works() public {
        vm.expectEmit(false, false, false, true);
        emit CollateralBpsUpdated(COLLATERAL_BPS, 2000);
        escrow.setCollateralBps(2000);

        assertEq(escrow.collateralBps(), 2000);
    }

    function test_setCollateralBps_onlyOwner_reverts() public {
        vm.prank(solver);
        vm.expectRevert();
        escrow.setCollateralBps(2000);
    }

    function test_setCollateralBps_invalidBps_reverts() public {
        vm.expectRevert(SolverEscrow.InvalidBps.selector);
        escrow.setCollateralBps(10_001);
    }

    // ============ distributeReward ============

    function _slashSolver(uint256 depositAmount, uint256 lockAmount) internal returns (bytes32 orderId) {
        orderId = keccak256("slashOrder");

        vm.startPrank(solver);
        token.approve(address(escrow), depositAmount);
        escrow.deposit(address(token), depositAmount);
        vm.stopPrank();

        vm.startPrank(authorizedCaller);
        escrow.lockCollateral(orderId, solver, address(token), lockAmount);
        escrow.slashCollateral(orderId);
        vm.stopPrank();
    }

    function test_distributeReward_works() public {
        // Slash 1000 tokens → escrow holds 1000 ERC6909
        _slashSolver(5000, 1000);

        address receiver = makeAddr("receiver");
        uint256 orderAmountIn = 10_000;
        uint256 expectedReward = (orderAmountIn * 150) / 10_000; // 1.5% = 150

        // Fund compact so allocatedTransfer can pay out underlying
        deal(address(token), address(compact), 10_000);

        vm.prank(authorizedCaller);
        vm.expectEmit(true, true, false, true);
        emit RewardDistributed(receiver, address(token), expectedReward);
        uint256 reward = escrow.distributeReward(address(token), orderAmountIn, receiver);

        assertEq(reward, expectedReward, "reward amount");
        assertEq(token.balanceOf(receiver), expectedReward, "receiver got underlying tokens");

        // Escrow ERC6909 decreased
        uint256 id = escrow.lockId(address(token));
        assertEq(compact.balanceOf(address(escrow), id), 1000 - expectedReward, "escrow ERC6909 decreased");
    }

    function test_distributeReward_exactSlashedBalance() public {
        // Slash exactly 150 tokens → 1.5% of 10_000 = 150 → exact match
        _slashSolver(5000, 150);
        deal(address(token), address(compact), 10_000);

        address receiver = makeAddr("receiver");
        uint256 id = escrow.lockId(address(token));

        vm.prank(authorizedCaller);
        uint256 reward = escrow.distributeReward(address(token), 10_000, receiver);

        assertEq(reward, 150, "exact match reward");
        assertEq(compact.balanceOf(address(escrow), id), 0, "escrow fully drained");
        assertEq(token.balanceOf(receiver), 150, "receiver got reward");
    }

    // ============ lockId ============

    function test_lockId_encoding() public view {
        uint256 expected = (uint256(uint96(lockTag)) << 160) | uint160(address(token));
        assertEq(escrow.lockId(address(token)), expected, "lockId encoding");
    }
}
