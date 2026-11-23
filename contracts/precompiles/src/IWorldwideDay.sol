// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IWorldwideDay {
    error FormingWindowOverflow();
    error OfferingWindowOverflow();
    error UtcPlus14TimestampOverflow();
    error DurationOverflow(string label);
    error DateStep(uint8 kind, uint32 dateKey);

    event WorldwideDayOpened(
        uint32 indexed worldwideDay, uint64 formingStart, uint64 formingEnd, uint64 offeringStart, uint64 offeringEnd
    );
    event WorldwideDaySealed(uint32 indexed worldwideDay);

    function getWorldwideDay(uint32 wwd)
        external
        view
        returns (uint32 day, uint64 formingStart, uint64 formingEnd, uint64 offeringsStart, uint64 offeringEnd);
    function activeWorldwideDays() external view returns (uint32[] memory wwds);
}
