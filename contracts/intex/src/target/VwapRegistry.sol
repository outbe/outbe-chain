// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

import {IOriginRouter} from "../origin/interfaces/IOriginRouter.sol";
import {IVwapRegistry} from "./interfaces/IVwapRegistry.sol";
import {IVwapSource} from "../shared/interfaces/IVwapSource.sol";

/// @title VwapRegistry
/// @author Outbe
/// @notice Target-chain record of the Oracle's finalized daily VWAPs.
contract VwapRegistry is IVwapRegistry, AccessControlUpgradeable, UUPSUpgradeable {
    /// @dev One slot per currency, read and written once per recorded row. `last` keeps the low
    ///      bytes, where the plain `lastUtcDay` value used to sit.
    struct RecordedDays {
        uint32 last;
        uint32 first;
    }

    /// @custom:storage-location erc7201:outbe.intex.VwapRegistry
    struct VwapRegistryStorage {
        address router;
        mapping(uint32 utcDay => mapping(uint16 isoCode => uint64 vwapMinor)) vwap;
        mapping(uint16 isoCode => RecordedDays) recorded;
        mapping(uint32 month => mapping(uint16 isoCode => uint64 vwapMinor)) monthMax;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.VwapRegistry")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xfec2335c4ea478059fd4eb47f547db9783936a7cf770c12fa3aad830c9629600;

    function _vs() private pure returns (VwapRegistryStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    function initialize(address admin, address router_) external initializer {
        if (admin == address(0)) revert ZeroAddress("admin");
        __AccessControl_init();
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _setRouter(router_);
    }

    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    /// @inheritdoc IVwapRegistry
    function setRouter(address router_) external onlyRole(DEFAULT_ADMIN_ROLE) {
        _setRouter(router_);
    }

    function _setRouter(address router_) private {
        if (router_ == address(0)) revert ZeroAddress("router");
        _vs().router = router_;
        emit RouterSet(router_);
    }

    /// @inheritdoc IVwapRegistry
    function record(uint32 utcDay, IOriginRouter.DailyVwap[] calldata rows) external {
        VwapRegistryStorage storage $ = _vs();
        if (msg.sender != $.router) revert NotRouter(msg.sender);
        for (uint256 i = 0; i < rows.length; ++i) {
            uint16 isoCode = rows[i].isoCode;
            uint64 vwapMinor = rows[i].vwapMinor;
            uint64 stored = $.vwap[utcDay][isoCode];
            if (stored != vwapMinor) {
                // A finalized day never changes, so a second price for it is a fault, not an update.
                if (stored != 0) revert DayAlreadyRecorded(utcDay, isoCode);
                $.vwap[utcDay][isoCode] = vwapMinor;
                if (vwapMinor > $.monthMax[utcDay / 100][isoCode]) $.monthMax[utcDay / 100][isoCode] = vwapMinor;
            }
            RecordedDays memory recorded = $.recorded[isoCode];
            if (utcDay > recorded.last || recorded.first == 0 || utcDay < recorded.first) {
                if (utcDay > recorded.last) recorded.last = utcDay;
                if (recorded.first == 0 || utcDay < recorded.first) recorded.first = utcDay;
                $.recorded[isoCode] = recorded;
            }
        }
        emit DailyVwapRecorded(utcDay, rows.length);
    }

    /// @inheritdoc IVwapRegistry
    function vwapOf(uint32 utcDay, uint16 isoCode) external view returns (uint64) {
        return _vs().vwap[utcDay][isoCode];
    }

    /// @inheritdoc IVwapRegistry
    function lastUtcDay(uint16 isoCode) external view returns (uint32) {
        return _vs().recorded[isoCode].last;
    }

    /// @inheritdoc IVwapRegistry
    function router() external view returns (address) {
        return _vs().router;
    }

    /// @inheritdoc IVwapSource
    /// @dev Reads the first month day by day and every later month once, from no earlier than the
    ///      first recorded day, so the cost follows the history's length in months.
    function maxUtcDayVwapSince(uint16 isoCode, uint32 fromUtcDay) external view returns (uint256 max) {
        if (fromUtcDay == 0) return 0;
        VwapRegistryStorage storage $ = _vs();
        RecordedDays memory recorded = $.recorded[isoCode];
        uint32 last = recorded.last;
        uint32 from = fromUtcDay > recorded.first ? fromUtcDay : recorded.first;
        if (last < from) return 0;
        uint32 firstMonth = from / 100;
        uint32 lastMonth = last / 100;
        max = _dayMax($, isoCode, firstMonth, from % 100, firstMonth == lastMonth ? last % 100 : 31);
        for (uint32 month = _nextMonth(firstMonth); month < lastMonth + 1; month = _nextMonth(month)) {
            uint64 monthMax = $.monthMax[month][isoCode];
            if (monthMax > max) max = monthMax;
        }
    }

    function _dayMax(VwapRegistryStorage storage $, uint16 isoCode, uint32 month, uint32 firstDd, uint32 lastDd)
        private
        view
        returns (uint256 max)
    {
        for (uint32 dd = firstDd; dd < lastDd + 1 && dd < 32; ++dd) {
            uint64 vwap = $.vwap[month * 100 + dd][isoCode];
            if (vwap > max) max = vwap;
        }
    }

    function _nextMonth(uint32 month) private pure returns (uint32) {
        return month % 100 == 12 ? (month / 100 + 1) * 100 + 1 : month + 1;
    }

    function supportsInterface(bytes4 interfaceId) public view override returns (bool) {
        return interfaceId == type(IVwapRegistry).interfaceId || interfaceId == type(IVwapSource).interfaceId
            || super.supportsInterface(interfaceId);
    }
}
