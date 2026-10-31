// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";

/// @dev Builds a `CreateSeriesParams` for a given emission day. Currencies default to USD (840).
///      Prices/promis carry non-zero defaults.
library CreateSeriesLib {
    /// @dev The readable id of that day's USD/USD series, e.g. `20260212-USD-U`.
    function seriesId(uint32 emissionDay) internal pure returns (bytes14) {
        bytes memory out = "00000000-USD-U";
        for (uint256 i = 8; i > 0; --i) {
            out[i - 1] = bytes1(uint8(48 + (emissionDay % 10)));
            emissionDay /= 10;
        }
        return bytes14(out);
    }

    function params(uint32 emissionDay, uint32 issuedUnits, uint32 callNoticePeriod)
        internal
        view
        returns (IIntexNFT1155.CreateSeriesParams memory)
    {
        return IIntexNFT1155.CreateSeriesParams({
            seriesId: seriesId(emissionDay),
            emissionDay: emissionDay,
            issuedAt: uint64(block.timestamp),
            issuanceCurrency: 840,
            referenceCurrency: 840,
            issuedUnits: issuedUnits,
            promisLoadMinor: 100_000 * 1e6,
            entryPriceMinor: 1e6,
            callPriceMinor: 200,
            callTrigger: IIntexNFT1155.IntexCallTrigger({
                callWindow: 0, callThreshold: 0, callNoticePeriod: callNoticePeriod, transitMargin: 1 hours
            })
        });
    }
}
