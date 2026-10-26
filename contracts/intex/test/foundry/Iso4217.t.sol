// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Iso4217} from "@contracts/shared/libs/Iso4217.sol";

contract Iso4217Test is Test {
    function test_AcceptsIsoCodesFromFirstToLast() public pure {
        assertTrue(Iso4217.isIso4217(8));
        assertTrue(Iso4217.isIso4217(840));
        assertTrue(Iso4217.isIso4217(999));
    }

    function test_RejectsCodesOutsideIso() public pure {
        assertFalse(Iso4217.isIso4217(0));
        assertFalse(Iso4217.isIso4217(1));
        assertFalse(Iso4217.isIso4217(975));
        assertFalse(Iso4217.isIso4217(1000));
    }

    function test_AlphaOf() public pure {
        _assertAlpha(840, "USD");
        _assertAlpha(32, "ARS");
        _assertAlpha(978, "EUR");
        (bool found, bytes3 alpha) = Iso4217.alphaOf(975);
        assertFalse(found);
        assertEq(alpha, bytes3(0));
    }

    function _assertAlpha(uint16 code, bytes3 expected) private pure {
        (bool found, bytes3 alpha) = Iso4217.alphaOf(code);
        assertTrue(found);
        assertEq(alpha, expected);
    }
}
