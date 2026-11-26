// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC6909} from "@openzeppelin/contracts/interfaces/IERC6909.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IEscrowAdapter} from "@contracts/target/interfaces/IEscrowAdapter.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {MockERC20} from "@test-mocks/MockERC20.sol";
import {MockWCOEN} from "@test-mocks/MockWCOEN.sol";

/// @dev Runs against the canonical Compact, set up from the genesis predeploy code. Its lock id
///      depends on the token, so a rotated token really lands in a separate position.
contract EscrowAdapterAssetVersionTest is Test {
    address internal constant COMPACT = 0x00000000000000171ede64904551eeDF3C6C9788;
    address internal constant COMPACT_2 = address(0xC0DE2);
    uint32 internal constant DAY_1 = 20260501;
    uint32 internal constant DAY_2 = 20260502;
    uint128 internal constant LOCK_UNITS = 1000e6;
    // MockWCOEN has 18 decimals: 1e12 token units per protocol unit.
    uint128 internal constant LOCK = LOCK_UNITS * 1e12;

    EscrowAdapter internal escrow;
    MockWCOEN internal tokenA;
    MockWCOEN internal tokenB;
    address internal admin = address(this);
    address internal proceeds = makeAddr("proceeds");
    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");

    function setUp() public {
        bytes memory code = vm.parseBytes(vm.trim(vm.readFile("../predeploys/the_compact.code.hex")));
        vm.etch(COMPACT, code);
        vm.etch(COMPACT_2, code);

        tokenA = new MockWCOEN();
        tokenB = new MockWCOEN();
        escrow = DeployProxy.escrowAdapter(admin, admin, proceeds, COMPACT, address(tokenA));
    }

    function _lock(uint32 day, address who, IERC20 token) internal {
        deal(address(token), who, LOCK);
        vm.prank(who);
        token.approve(address(escrow), type(uint256).max);
        escrow.lockIntexAuctionAsset(day, who, LOCK_UNITS, 1_000_000, 1);
    }

    function _positionOf(address compactAt, uint256 lockId) internal view returns (uint256) {
        return IERC6909(compactAt).balanceOf(address(escrow), lockId);
    }

    function test_RotatingWithLiveLocksRetiresTheActiveAsset() public {
        _lock(DAY_1, alice, tokenA);
        uint256 lockIdA = escrow.lockId();

        vm.expectEmit(true, false, false, true, address(escrow));
        emit IEscrowAdapter.IntexAuctionAssetVersionRetired(0, COMPACT, address(tokenA), lockIdA);
        escrow.setIntexAuctionAsset(COMPACT, address(tokenB));

        assertEq(escrow.currentIntexAuctionAssetVersion(), 1, "the active asset moved on");
        IEscrowAdapter.IntexAuctionAssetVersion memory retired = escrow.getIntexAuctionAssetVersion(0);
        assertEq(retired.compact, COMPACT, "retired compact");
        assertEq(address(retired.asset), address(tokenA), "retired token");
        assertEq(retired.lockId, lockIdA, "retired lock id");
        assertEq(escrow.lockId(), 0, "the new asset bootstraps its own lock");
        assertEq(
            address(escrow.getIntexAuctionAssetVersion(1).asset), address(tokenB), "active version reads the wiring"
        );
        assertEq(_positionOf(COMPACT, lockIdA), LOCK, "the locked funds stayed where they were");
    }

    function test_ADayLockedBeforeARotationIsClaimedInItsOwnAsset() public {
        _lock(DAY_1, alice, tokenA);
        uint256 lockIdA = escrow.lockId();
        escrow.setIntexAuctionAsset(COMPACT, address(tokenB));

        vm.warp(block.timestamp + escrow.UNFINALIZED_RETURN_DELAY());
        escrow.returnIntexAuctionAsset(DAY_1, alice);

        assertEq(tokenA.balanceOf(alice), LOCK, "refunded in the token it was locked in");
        assertEq(tokenB.balanceOf(alice), 0, "never in the rotated token");
        assertEq(_positionOf(COMPACT, lockIdA), 0, "out of its own position");
    }

    function test_ADayLockedBeforeARotationFinalizesInItsOwnAsset() public {
        _lock(DAY_1, alice, tokenA);
        _lock(DAY_1, bob, tokenA);
        escrow.setIntexAuctionAsset(COMPACT, address(tokenB));
        vm.warp(block.timestamp + 5 minutes);

        address[] memory winners = new address[](1);
        winners[0] = bob;
        vm.prank(proceeds);
        escrow.chargeIntexAuctionAsset(DAY_1, winners, 0, 0, 500_000, 1000e6, true);
        escrow.returnIntexAuctionAsset(DAY_1, alice);
        escrow.returnIntexAuctionAsset(DAY_1, bob);

        assertEq(tokenA.balanceOf(alice), LOCK, "loser refunded in the day's token");
        assertEq(tokenA.balanceOf(bob), LOCK / 2, "winner refunded in the day's token");
        assertEq(tokenA.balanceOf(proceeds), LOCK / 2, "proceeds leave in the day's token");
        assertEq(tokenB.balanceOf(proceeds), 0, "not in the rotated token");
    }

    function test_ADayLockedBeforeARotationPaysAtItsOwnScale() public {
        _lock(DAY_1, bob, tokenA);
        escrow.setIntexAuctionAsset(COMPACT, address(new MockERC20("Token", "TKN", 6)));
        vm.warp(block.timestamp + 5 minutes);

        address[] memory winners = new address[](1);
        winners[0] = bob;
        vm.prank(proceeds);
        escrow.chargeIntexAuctionAsset(DAY_1, winners, 0, 0, 500_000, 1000e6, true);

        assertEq(tokenA.balanceOf(proceeds), LOCK / 2, "paid at the day's 18-decimal scale");
    }

    function test_ADayKeepsItsPaymentTokenAcrossARotation() public {
        _lock(DAY_1, alice, tokenA);
        escrow.setIntexAuctionAsset(COMPACT, address(tokenB));

        assertEq(address(escrow.dayIntexAuctionAsset(DAY_1)), address(tokenA), "the day's own token");
        assertEq(address(escrow.intexAuctionAsset()), address(tokenB), "the active token moved on");
    }

    function test_ADayAfterARotationLocksUnderTheNewAsset() public {
        _lock(DAY_1, alice, tokenA);
        uint256 lockIdA = escrow.lockId();
        escrow.setIntexAuctionAsset(COMPACT, address(tokenB));

        _lock(DAY_2, bob, tokenB);
        uint256 lockIdB = escrow.lockId();
        assertTrue(lockIdB != 0 && lockIdB != lockIdA, "a separate position");

        vm.warp(block.timestamp + escrow.UNFINALIZED_RETURN_DELAY());
        escrow.returnIntexAuctionAsset(DAY_2, bob);
        assertEq(tokenB.balanceOf(bob), LOCK, "refunded in the new token");
        assertEq(_positionOf(COMPACT, lockIdA), LOCK, "the old day's funds untouched");
    }

    function test_ALateLockIntoARetiredDayReverts() public {
        _lock(DAY_1, alice, tokenA);
        escrow.setIntexAuctionAsset(COMPACT, address(tokenB));

        deal(address(tokenB), bob, LOCK);
        vm.prank(bob);
        tokenB.approve(address(escrow), type(uint256).max);
        vm.expectRevert(abi.encodeWithSelector(IEscrowAdapter.DayIntexAuctionAssetRetired.selector, DAY_1, uint8(0)));
        escrow.lockIntexAuctionAsset(DAY_1, bob, LOCK_UNITS, 1_000_000, 1);
    }

    function test_ACommitBondTakenBeforeARotationIsReleasedInItsOwnAsset() public {
        deal(address(tokenA), alice, LOCK);
        vm.prank(alice);
        tokenA.approve(address(escrow), type(uint256).max);
        escrow.lockIntexAuctionBidBond(DAY_1, alice, LOCK_UNITS);
        assertEq(escrow.getIntexAuctionBidBond(DAY_1, alice).assetVersion, 0, "bond remembers its asset");

        escrow.setIntexAuctionAsset(COMPACT, address(tokenB));
        vm.warp(block.timestamp + 5 minutes);
        escrow.unlockIntexAuctionBidBond(DAY_1, alice, 0);

        assertEq(tokenA.balanceOf(alice), LOCK, "bond returned in the token it was taken in");
    }

    function test_RotatingTheCompactKeepsOldDaysOnTheOldCompact() public {
        _lock(DAY_1, alice, tokenA);
        uint256 lockIdOld = escrow.lockId();
        escrow.setIntexAuctionAsset(COMPACT_2, address(tokenA));

        _lock(DAY_2, bob, tokenA);
        uint256 lockIdNew = escrow.lockId();

        vm.warp(block.timestamp + escrow.UNFINALIZED_RETURN_DELAY());
        escrow.returnIntexAuctionAsset(DAY_1, alice);
        escrow.returnIntexAuctionAsset(DAY_2, bob);

        assertEq(tokenA.balanceOf(alice), LOCK, "old day refunded");
        assertEq(tokenA.balanceOf(bob), LOCK, "new day refunded");
        assertEq(_positionOf(COMPACT, lockIdOld), 0, "old day withdrew from the old compact");
        assertEq(_positionOf(COMPACT_2, lockIdNew), 0, "new day withdrew from the new compact");
    }
}
