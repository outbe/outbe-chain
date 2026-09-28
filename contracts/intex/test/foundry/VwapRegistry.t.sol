// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {IVwapRegistry} from "@contracts/target/interfaces/IVwapRegistry.sol";
import {IVwapSource} from "@contracts/shared/interfaces/IVwapSource.sol";
import {VwapRegistry} from "@contracts/target/VwapRegistry.sol";
import {DateKey} from "@contracts/shared/libs/DateKey.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";

contract VwapRegistryTest is Test {
    uint16 internal constant USD = 840;
    uint16 internal constant EUR = 978;

    address internal admin = makeAddr("admin");
    address internal router = makeAddr("router");
    address internal stranger = makeAddr("stranger");

    VwapRegistry internal registry;

    function setUp() public {
        registry = DeployProxy.vwapRegistry(admin, router);
    }

    function _rows(uint16 isoCode, uint64 vwapMinor) internal pure returns (IOriginRouter.DailyVwap[] memory rows) {
        rows = new IOriginRouter.DailyVwap[](1);
        rows[0] = IOriginRouter.DailyVwap({isoCode: isoCode, vwapMinor: vwapMinor});
    }

    function _record(uint32 utcDay, uint16 isoCode, uint64 vwapMinor) internal {
        vm.prank(router);
        registry.record(utcDay, _rows(isoCode, vwapMinor));
    }

    function test_Initialize_SetsAdminAndRouter() public view {
        assertTrue(registry.hasRole(registry.DEFAULT_ADMIN_ROLE(), admin));
        assertEq(registry.router(), router);
    }

    function test_RevertWhen_InitializeCalledTwice() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        registry.initialize(stranger, stranger);
    }

    function test_RevertWhen_ImplementationInitialized() public {
        VwapRegistry impl = new VwapRegistry();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        impl.initialize(admin, router);
    }

    function test_RevertWhen_InitializeZeroAdmin() public {
        VwapRegistry impl = new VwapRegistry();
        vm.expectRevert(abi.encodeWithSelector(IVwapRegistry.ZeroAddress.selector, "admin"));
        new ERC1967Proxy(address(impl), abi.encodeCall(VwapRegistry.initialize, (address(0), router)));
    }

    function test_RevertWhen_InitializeZeroRouter() public {
        VwapRegistry impl = new VwapRegistry();
        vm.expectRevert(abi.encodeWithSelector(IVwapRegistry.ZeroAddress.selector, "router"));
        new ERC1967Proxy(address(impl), abi.encodeCall(VwapRegistry.initialize, (admin, address(0))));
    }

    function test_RevertWhen_UpgradeByNonAdmin() public {
        VwapRegistry newImpl = new VwapRegistry();
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        registry.upgradeToAndCall(address(newImpl), "");
    }

    function test_SetRouter_OnlyAdmin() public {
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(IAccessControl.AccessControlUnauthorizedAccount.selector, stranger, bytes32(0))
        );
        registry.setRouter(stranger);

        vm.expectEmit(address(registry));
        emit IVwapRegistry.RouterSet(stranger);
        vm.prank(admin);
        registry.setRouter(stranger);
        assertEq(registry.router(), stranger);
    }

    function test_RevertWhen_RecordByNonRouter() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(IVwapRegistry.NotRouter.selector, stranger));
        registry.record(20260315, _rows(USD, 1_500_000));
    }

    function test_Record_StoresEveryRowAndTheNewestDay() public {
        IOriginRouter.DailyVwap[] memory rows = new IOriginRouter.DailyVwap[](2);
        rows[0] = IOriginRouter.DailyVwap({isoCode: USD, vwapMinor: 1_500_000});
        rows[1] = IOriginRouter.DailyVwap({isoCode: EUR, vwapMinor: 1_400_000});
        vm.expectEmit(address(registry));
        emit IVwapRegistry.DailyVwapRecorded(20260315, 2);
        vm.prank(router);
        registry.record(20260315, rows);

        assertEq(registry.vwapOf(20260315, USD), 1_500_000);
        assertEq(registry.vwapOf(20260315, EUR), 1_400_000);
        assertEq(registry.lastUtcDay(USD), 20260315);
        assertEq(registry.lastUtcDay(EUR), 20260315);
    }

    /// @notice The transport is unordered: an older day landing late fills its gap without moving the newest.
    function test_Record_AnOlderDayArrivingLateKeepsTheNewest() public {
        _record(20260315, USD, 1_500_000);
        _record(20260313, USD, 1_300_000);
        assertEq(registry.lastUtcDay(USD), 20260315);
        assertEq(registry.vwapOf(20260313, USD), 1_300_000);
    }

    function test_MaxUtcDayVwapSince_TakesTheHighestDayInRange() public {
        _record(20260310, USD, 2_000_000);
        _record(20260312, USD, 1_600_000);
        _record(20260314, USD, 1_100_000);
        _record(20260315, USD, 1_200_000);
        _record(20260313, EUR, 9_000_000);

        assertEq(registry.maxUtcDayVwapSince(USD, 20260312), 1_600_000, "an earlier day is out of range");
        assertEq(registry.maxUtcDayVwapSince(USD, 20260310), 2_000_000, "the first day is in range");
        assertEq(registry.maxUtcDayVwapSince(USD, 20260316), 0, "nothing after the newest day");
        assertEq(registry.maxUtcDayVwapSince(USD, 0), 0);
        assertEq(registry.maxUtcDayVwapSince(392, 20260301), 0, "a currency never recorded");
    }

    function test_MaxUtcDayVwapSince_WalksBackAcrossMonthsAndYears() public {
        _record(20251231, USD, 3_000_000);
        _record(20240229, USD, 5_000_000);
        _record(20260102, USD, 1_000_000);

        assertEq(registry.maxUtcDayVwapSince(USD, 20260101), 1_000_000);
        assertEq(registry.maxUtcDayVwapSince(USD, 20251231), 3_000_000);
        assertEq(registry.maxUtcDayVwapSince(USD, 20240229), 5_000_000);
    }

    function test_Record_SamePriceAgainIsIdempotent() public {
        _record(20260315, USD, 1_500_000);
        _record(20260315, USD, 1_500_000);
        assertEq(registry.vwapOf(20260315, USD), 1_500_000);
    }

    function test_RevertWhen_RecordedDayArrivesWithAnotherPrice() public {
        _record(20260315, USD, 1_500_000);
        vm.expectRevert(abi.encodeWithSelector(IVwapRegistry.DayAlreadyRecorded.selector, uint32(20260315), USD));
        _record(20260315, USD, 1_400_000);
        assertEq(registry.vwapOf(20260315, USD), 1_500_000);
    }

    /// @notice A start long before the first recorded day reads from that day on, not from year zero.
    function test_MaxUtcDayVwapSince_AnEarlyStartReadsFromTheFirstRecordedDay() public {
        _record(20260310, USD, 2_000_000);
        _record(20260402, USD, 1_000_000);
        assertEq(registry.maxUtcDayVwapSince(USD, 101), 2_000_000);
    }

    /// @notice Month-bucketed reads return exactly what a walk over every recorded day would.
    function test_MaxUtcDayVwapSince_MatchesAWalkOverEveryDay() public {
        uint32[] memory days_ = new uint32[](180);
        uint64[] memory prices = new uint64[](180);
        uint256 n;
        uint256 seed = 0x2545f491;
        for (uint32 year = 2024; year <= 2026; ++year) {
            for (uint32 month = 1; month <= 12; ++month) {
                uint32[5] memory dds = [uint32(1), 9, 15, 28, 31];
                for (uint256 k = 0; k < 5; ++k) {
                    seed = uint256(keccak256(abi.encode(seed)));
                    days_[n] = year * 10_000 + month * 100 + dds[k];
                    // One day before a start inside its month outranks every other price.
                    prices[n] = days_[n] == 20250601 ? 88_888 : uint64(1 + seed % 10_000);
                    _record(days_[n], USD, prices[n]);
                    ++n;
                }
            }
        }
        uint32[10] memory froms = [
            uint32(20230101), 20240101, 20240131, 20240201, 20241231, 20250101, 20250615, 20251231, 20261231, 20270101
        ];
        for (uint256 f = 0; f < froms.length; ++f) {
            uint256 expected;
            for (uint256 i = 0; i < n; ++i) {
                if (days_[i] >= froms[f] && prices[i] > expected) expected = prices[i];
            }
            assertEq(registry.maxUtcDayVwapSince(USD, froms[f]), expected);
        }
    }

    function test_SupportsInterface_VwapSource() public view {
        assertTrue(registry.supportsInterface(type(IVwapRegistry).interfaceId));
        assertTrue(registry.supportsInterface(type(IVwapSource).interfaceId));
        assertTrue(registry.supportsInterface(type(IAccessControl).interfaceId));
    }
}

contract DateKeyTest is Test {
    function test_PreviousDateKey_InsideAMonth() public pure {
        assertEq(DateKey.previousDateKey(20260315), 20260314);
    }

    function test_PreviousDateKey_AcrossMonthEnds() public pure {
        assertEq(DateKey.previousDateKey(20260501), 20260430);
        assertEq(DateKey.previousDateKey(20260801), 20260731);
        assertEq(DateKey.previousDateKey(20260101), 20251231);
    }

    function test_FirstFullDay_IsTheIssuanceDayOnlyAtExactMidnight() public pure {
        assertEq(DateKey.firstFullDay(1_772_323_200), 20260301, "2026-03-01 00:00:00");
        assertEq(DateKey.firstFullDay(1_772_323_201), 20260302);
        assertEq(DateKey.firstFullDay(1_772_323_199), 20260301, "one second before midnight");
        assertEq(DateKey.firstFullDay(1_709_164_800), 20240229, "a leap day");
        assertEq(DateKey.firstFullDay(1_767_139_201), 20260101, "across a year");
        assertEq(DateKey.firstFullDay(0), 19700101);
    }

    function test_PreviousDateKey_AcrossFebruary() public pure {
        assertEq(DateKey.previousDateKey(20260301), 20260228);
        assertEq(DateKey.previousDateKey(20240301), 20240229);
        assertEq(DateKey.previousDateKey(21000301), 21000228, "a century is not a leap year");
        assertEq(DateKey.previousDateKey(20000301), 20000229, "unless it divides by 400");
    }
}
