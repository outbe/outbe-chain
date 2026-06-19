// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {IVaultProvider} from "@contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol";
import {MockTheCompact} from "@test-mocks/MockTheCompact.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockSettlementVault} from "@test-mocks/MockSettlementVault.sol";
import {MockVaultProvider} from "@test-mocks/MockVaultProvider.sol";

/// @dev ERC20 that skims a fee on every move: the sender is crosschainBurned the full amount but the
///      recipient is crosschainMinted amount minus fee. Breaks the "exactly `amount` lands" assumption.
contract FeeOnTransferToken is IERC20 {
    mapping(address => uint256) private _balances;
    mapping(address => mapping(address => uint256)) private _allowances;
    uint256 private _supply;
    uint256 public immutable feeBps;

    string public constant name = "Fee Token";
    string public constant symbol = "FEE";
    uint8 public constant decimals = 6;

    constructor(uint256 _feeBps) {
        feeBps = _feeBps;
    }

    function totalSupply() external view returns (uint256) {
        return _supply;
    }

    function balanceOf(address a) external view returns (uint256) {
        return _balances[a];
    }

    function allowance(address o, address s) external view returns (uint256) {
        return _allowances[o][s];
    }

    function approve(address s, uint256 a) external returns (bool) {
        _allowances[msg.sender][s] = a;
        return true;
    }

    function mint(address to, uint256 a) external {
        _balances[to] += a;
        _supply += a;
    }

    function transfer(address to, uint256 a) external returns (bool) {
        _move(msg.sender, to, a);
        return true;
    }

    function transferFrom(address from, address to, uint256 a) external returns (bool) {
        _allowances[from][msg.sender] -= a;
        _move(from, to, a);
        return true;
    }

    function _move(address from, address to, uint256 a) internal {
        _balances[from] -= a;
        uint256 fee = (a * feeBps) / 10_000;
        _balances[to] += a - fee;
        _supply -= fee;
    }
}

/// @dev Re-enters EscrowAdapter.claimRefund from depositLiquidity to probe the nonReentrant guard.
contract HostileReentrantVaultProvider {
    EscrowAdapter public escrow;
    uint32 public seriesId;
    address public bidder;

    function arm(EscrowAdapter _escrow, uint32 _seriesId, address _bidder) external {
        escrow = _escrow;
        seriesId = _seriesId;
        bidder = _bidder;
    }

    function depositLiquidity(address, uint256) external returns (uint256) {
        escrow.claimRefund(seriesId, bidder);
        return 1;
    }
}

contract EscrowAdapterHardeningTest is Test {
    EscrowAdapter internal escrow;
    MockTheCompact internal compact;
    MockERC20 internal paymentToken;
    MockVaultProvider internal provider;

    address internal admin = address(1);
    address internal bridger = address(2);
    address internal auction = address(3);
    address internal bidderA = address(0xA);
    address internal bidderB = address(0xB);

    uint32 internal constant SERIES = 1;

    function setUp() public {
        escrow = DeployProxy.escrowAdapter(admin, bridger);
        compact = new MockTheCompact();
        paymentToken = new MockERC20("USD Coin", "USDC", 6);
        MockSettlementVault vault = new MockSettlementVault(address(paymentToken), "Mock Vault USDC", "mvUSDC", 6);
        provider = new MockVaultProvider();
        provider.addVault(vault);
        provider.addLiquiditySource(address(escrow), IVaultProvider.LiquiditySource.IntexBidPrice);

        vm.prank(admin);
        escrow.wire(auction, address(compact), address(provider), address(paymentToken));
        compact.setResetPeriodSeconds(0);
    }

    function _fund(address bidder, uint256 amount) internal {
        paymentToken.mint(bidder, amount);
        vm.prank(bidder);
        paymentToken.approve(address(escrow), type(uint256).max);
    }

    function test_Boundary_LockFunds_AcceptsUint64Max() public {
        uint64 max = type(uint64).max;
        _fund(bidderA, max);

        vm.prank(auction);
        escrow.lockFunds(SERIES, bidderA, max);

        (,, uint64 totalLocked) = escrow.getAuctionStatus(SERIES);
        assertEq(totalLocked, max, "totalLocked");
        assertEq(escrow.getBidLock(SERIES, bidderA).lockedAmount, max, "lockedAmount");
        assertEq(compact.balanceOf(address(escrow), escrow.lockId()), max, "pooled balance");
    }

    function test_Boundary_TotalLockedOverflow_Reverts() public {
        uint64 max = type(uint64).max;
        _fund(bidderA, max);
        _fund(bidderB, 1);

        vm.prank(auction);
        escrow.lockFunds(SERIES, bidderA, max);

        vm.prank(auction);
        vm.expectRevert(abi.encodeWithSignature("Panic(uint256)", 0x11));
        escrow.lockFunds(SERIES, bidderB, 1);

        (,, uint64 totalLocked) = escrow.getAuctionStatus(SERIES);
        assertEq(totalLocked, max, "totalLocked unchanged after overflow revert");
        assertEq(compact.balanceOf(address(escrow), escrow.lockId()), max, "pooled balance unchanged");
    }

    function test_FeeOnTransferToken_LockFunds_FailsClosed() public {
        EscrowAdapter feeEscrow = DeployProxy.escrowAdapter(admin, bridger);
        MockTheCompact feeCompact = new MockTheCompact();
        FeeOnTransferToken feeToken = new FeeOnTransferToken(100);
        MockVaultProvider feeProvider = new MockVaultProvider();

        vm.prank(admin);
        feeEscrow.wire(auction, address(feeCompact), address(feeProvider), address(feeToken));
        feeCompact.setResetPeriodSeconds(0);

        feeToken.mint(bidderA, 1_000e6);
        vm.prank(bidderA);
        feeToken.approve(address(feeEscrow), type(uint256).max);

        vm.prank(auction);
        vm.expectRevert();
        feeEscrow.lockFunds(SERIES, bidderA, 1_000e6);

        (,, uint64 totalLocked) = feeEscrow.getAuctionStatus(SERIES);
        assertEq(totalLocked, 0, "no state written on a fee-token lock");
    }

    function test_HostileReentrantVault_FinalizeBlocksReentry_ConservationHolds() public {
        EscrowAdapter hEscrow = DeployProxy.escrowAdapter(admin, bridger);
        MockTheCompact hCompact = new MockTheCompact();
        MockERC20 hToken = new MockERC20("USD Coin", "USDC", 6);
        HostileReentrantVaultProvider hostile = new HostileReentrantVaultProvider();

        vm.prank(admin);
        hEscrow.wire(auction, address(hCompact), address(hostile), address(hToken));
        hCompact.setResetPeriodSeconds(0);

        uint64 amount = 500e6;
        hToken.mint(bidderA, amount);
        vm.prank(bidderA);
        hToken.approve(address(hEscrow), type(uint256).max);

        vm.prank(auction);
        hEscrow.lockFunds(SERIES, bidderA, amount);

        hostile.arm(hEscrow, SERIES, bidderA);

        IEscrowAdapter.FinalizationInstruction[] memory ins = new IEscrowAdapter.FinalizationInstruction[](1);
        ins[0] = IEscrowAdapter.FinalizationInstruction({bidder: bidderA, refundedAmount: 0, paidAmount: amount});
        vm.prank(bridger);
        hEscrow.finalizeAuction(SERIES, bytes32(uint256(0x1)), ins);

        assertEq(
            uint8(hEscrow.getBidLock(SERIES, bidderA).status),
            uint8(IEscrowAdapter.LockStatus.Locked),
            "lock must stay Locked after the re-entry was blocked"
        );
        (,, uint64 totalLocked) = hEscrow.getAuctionStatus(SERIES);
        assertEq(totalLocked, amount, "totalLocked unchanged");
        assertEq(hCompact.balanceOf(address(hEscrow), hEscrow.lockId()), amount, "pooled balance unchanged");
    }
}
