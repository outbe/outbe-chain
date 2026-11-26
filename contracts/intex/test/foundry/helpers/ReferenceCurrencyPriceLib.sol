// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";

/// @dev A one-currency price table, the shape every pre-multicurrency fixture used.
library ReferenceCurrencyPriceLib {
    function one(uint16 isoCode, uint64 entry, uint64 callPrice)
        internal
        pure
        returns (IIntexAuction.ReferenceCurrencyPrice[] memory rows)
    {
        rows = new IIntexAuction.ReferenceCurrencyPrice[](1);
        rows[0] =
            IIntexAuction.ReferenceCurrencyPrice({isoCode: isoCode, entryPriceMinor: entry, callPriceMinor: callPrice});
    }

    function onePriced(uint16 isoCode, uint64 entry, uint64 callPrice)
        internal
        pure
        returns (IIntexAuction.ReferenceCurrencyPrice[] memory rows)
    {
        rows = new IIntexAuction.ReferenceCurrencyPrice[](1);
        rows[0] =
            IIntexAuction.ReferenceCurrencyPrice({isoCode: isoCode, entryPriceMinor: entry, callPriceMinor: callPrice});
    }
}
