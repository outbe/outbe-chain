// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";

/// @dev A one-currency price table, the shape every pre-multicurrency fixture used.
library ReferencePriceLib {
    function one(uint16 isoCode, uint64 entry, uint64 floorPrice, uint64 callPrice)
        internal
        pure
        returns (IOriginRouter.ReferencePrice[] memory rows)
    {
        rows = new IOriginRouter.ReferencePrice[](1);
        rows[0] = IOriginRouter.ReferencePrice({
            isoCode: isoCode, entryPriceMinor: entry, floorPriceMinor: floorPrice, callPriceMinor: callPrice
        });
    }
}
