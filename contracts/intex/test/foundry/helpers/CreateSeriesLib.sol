// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";

/// @dev Builds a `CreateSeriesParams` for a given worldwide day. `seriesId` is derived from the day
///      (identity while one series per day). Currencies default to USD (840); prices/promis carry non-zero defaults.
library CreateSeriesLib {
    function params(uint32 worldwideDay, uint32 issuedIntexCount, uint32 intexCallPeriod)
        internal
        pure
        returns (IIntexNFT1155.CreateSeriesParams memory)
    {
        return IIntexNFT1155.CreateSeriesParams({
            seriesId: worldwideDay, // derived from the day
            worldwideDay: worldwideDay,
            issuanceCurrency: 840,
            referenceCurrency: 840,
            issuedIntexCount: issuedIntexCount,
            promisLoadMinor: 100_000 * 1e18,
            entryPriceMinor: 1e13,
            floorPriceMinor: 100,
            callPriceMinor: 200,
            callTrigger: IIntexNFT1155.IntexCallTrigger({
                windowDays: 0, thresholdDays: 0, intexCallPeriod: intexCallPeriod
            })
        });
    }
}
