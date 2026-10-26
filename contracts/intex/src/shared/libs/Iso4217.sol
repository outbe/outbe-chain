// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// @title Iso4217
/// @author Outbe
/// @notice ISO 4217 currency codes from the SIX List One snapshot published 2026-01-01, the same
///         snapshot the Outbe chain validates against.
library Iso4217 {
    /// @dev Sorted numeric codes, two bytes each, positionally aligned with `ALPHA`.
    // forgefmt: disable-next-item
    bytes private constant NUMERIC =
        hex"0008000c00200024002c0030003200330034003c"
        hex"0040004400480054005a00600068006c0074007c"
        hex"0084008800900098009c00aa00ae00bc00c000cb"
        hex"00d000d600de00e600e800ee00f20106010e0124"
        hex"014001440148014c01540158015c016001640168"
        hex"016c0170017801840188018c018e019001940198"
        hex"019a019e01a101a201a601aa01ae01b201be01c6"
        hex"01ca01ce01e001e401f001f201f802000204020c"
        hex"021402150224022a022e02360242024a024e0256"
        hex"0258025c0260027a02830286028e02aa02b202be"
        hex"02c002c202c602d802ec02f002f402f802fc0308"
        hex"030c03100314032003270332033a03420348035a"
        hex"035c037203760385039c039d039e039f03a003a1"
        hex"03a203a503a603a803aa03ac03ad03af03b003b2"
        hex"03b303b403b503b603b703b803b903bb03bc03bd"
        hex"03be03bf03c003c103c203c303c403c503c703c8"
        hex"03c903ca03cb03cc03cd03d003d103d203d303d4"
        hex"03d503d803d903da03de03e203e503e7";
    /// @dev Alpha-3 codes, three bytes each.
    // forgefmt: disable-next-item
    bytes private constant ALPHA =
        "ALLDZDARSAUDBSDBHDBDTAMDBBDBMD"
        "BTNBOBBWPBZDSBDBNDMMKBIFKHRCAD"
        "CVEKYDLKRCLPCNYCOPKMFCRCCUPCZK"
        "DKKDOPSVCETBERNFKPFJDDJFGMDGIP"
        "GTQGNFGYDHTGHNLHKDHUFISKINRIDR"
        "IRRIQDILSJMDJPYXADKZTJODKESKPW"
        "KRWKWDKGSLAKLBPLSLLRDLYDMOPMWK"
        "MYRMVRMURMXNMNTMDLMADOMRNADNPR"
        "XCGAWGVUVNZDNIONGNNOKPKRPABPGK"
        "PYGPENPHPQARRUBRWFSHPSARSCRSGD"
        "VNDSOSZARSSPSZLSEKCHFSYPTHBTOP"
        "TTDAEDTNDUGXMKDEGPGBPTZSUSDUYU"
        "UZSWSTYERTWDZWGSLEVEDUYWVESMRU"
        "STNBYNTMTGHSSDGUYIRSDMZNAZNRON"
        "CHECHWTRYXAFXCDXOFXPFXBAXBBXBC"
        "XBDXAUXDRXAGXPTXTSXPDXUAZMWSRD"
        "MGACOUAFNTJSAOACDFBAMEURMXVUAH"
        "GELBOVPLNBRLCLFXSUUSNXXX";

    /// @notice Whether `code` is an ISO 4217 numeric code.
    /// @param code Numeric currency code.
    /// @return found True when the snapshot carries `code`.
    function isIso4217(uint16 code) internal pure returns (bool found) {
        (found,) = alphaOf(code);
    }

    /// @notice The alpha-3 code of an ISO 4217 numeric code.
    /// @param code Numeric currency code.
    /// @return found False when `code` is not an ISO 4217 code.
    /// @return alpha The alpha-3 code, zero when not found.
    function alphaOf(uint16 code) internal pure returns (bool found, bytes3 alpha) {
        bytes memory numeric = NUMERIC;
        uint256 lo = 0;
        uint256 hi = numeric.length / 2;
        while (lo < hi) {
            uint256 mid = (lo + hi) / 2;
            uint16 at = (uint16(uint8(numeric[2 * mid])) << 8) | uint16(uint8(numeric[2 * mid + 1]));
            if (at < code) {
                lo = mid + 1;
            } else if (at > code) {
                hi = mid;
            } else {
                bytes memory letters = ALPHA;
                uint256 at3 = 3 * mid;
                return (true, bytes3(letters[at3]) | (bytes3(letters[at3 + 1]) >> 8) | (bytes3(letters[at3 + 2]) >> 16));
            }
        }
    }
}
