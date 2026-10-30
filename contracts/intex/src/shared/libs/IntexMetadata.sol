// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Strings} from "@openzeppelin/contracts/utils/Strings.sol";
import {Base64} from "@openzeppelin/contracts/utils/Base64.sol";
import {IIntexNFT1155} from "../interfaces/IIntexNFT1155.sol";
import {BridgeMsgCodec} from "./BridgeMsgCodec.sol";
import {DateKey} from "./DateKey.sol";
import {Iso4217} from "./Iso4217.sol";

/// @title IntexMetadata
/// @notice JSON + SVG metadata rendering for IntexNFT1155 tokens.
/// @dev External linked library: deployed once and delegatecalled, keeping the renderer's
///      bytecode outside IntexNFT1155's EIP-170 budget. All functions are pure. The caller
///      passes a SeriesData snapshot whose state already carries the derived Forfeited.
library IntexMetadata {
    string internal constant COLLECTION_DESCRIPTION =
        "Intex is the mining right of the Outbe network, issued in series through an emission day auction across all connected chains. Units stay transferable until the settlement deadline of a called series. A settlement on Outbe burns them here and moves them to the Settled Intex collection of the Outbe Intex precompile.";

    /// @dev First row baseline of the card and the step between rows.
    uint256 private constant ROW_TOP = 265;
    uint256 private constant ROW_STEP = 45;

    /// @dev Every COEN/ISO price arrives directly in six-decimal ISO stable-units.
    uint8 private constant PRICE_DECIMALS = BridgeMsgCodec.PROTOCOL_DECIMALS;
    uint8 private constant PRICE_PRECISION = 6;
    /// @dev Shared numeric scale carried by six-decimal monetary fields.
    uint256 private constant SCALE_1E6 = 10 ** BridgeMsgCodec.PROTOCOL_DECIMALS;

    /// @notice Build the `data:application/json;base64,...` URI for a token.
    /// @param data Series record for the token id, with its effective state. The
    ///        caller derives `Forfeited`, so this library never re-derives it.
    /// @return Token URI, or the collection document when the record does not exist
    ///         (`issuedAt == 0`).
    function tokenURI(IIntexNFT1155.SeriesData memory data) external pure returns (string memory) {
        if (data.issuedAt == 0) return _collectionURI();

        string memory displayId = _displayId(data);

        string memory json = string.concat(
            "{\"name\":\"Intex ",
            displayId,
            "\",\"description\":\"",
            _tokenDescription(data, displayId),
            "\",\"image\":\"data:image/svg+xml;base64,",
            Base64.encode(bytes(_generateSVG(data, displayId))),
            "\",\"attributes\":",
            _buildAttributes(data),
            "}"
        );

        return string.concat("data:application/json;base64,", Base64.encode(bytes(json)));
    }

    /// @notice Collection-level document (ERC-7572 contractURI), base64 data URI.
    function contractURI() external pure returns (string memory) {
        return _collectionURI();
    }

    function _collectionURI() private pure returns (string memory) {
        return string.concat(
            "data:application/json;base64,",
            Base64.encode(bytes(string.concat("{\"name\":\"Intex\",\"description\":\"", COLLECTION_DESCRIPTION, "\"}")))
        );
    }

    function _tokenDescription(IIntexNFT1155.SeriesData memory data, string memory displayId)
        private
        pure
        returns (string memory)
    {
        return string.concat(
            "Units of Intex series ",
            displayId,
            " from the Emission Day ",
            Strings.toString(data.emissionDay),
            " auction. A unit is a mining right: its owner pays the settlement cost at the series'",
            " fixed entry price. Units stay transferable until the settlement deadline of a called series."
        );
    }

    /// @dev The readable series id: `20260212-TRY-U`, or `20261102-EUR` without its two
    ///      `0x00` padding bytes.
    function _displayId(IIntexNFT1155.SeriesData memory data) private pure returns (string memory) {
        bytes14 id = data.seriesId;
        uint256 len = 14;
        while (len > 0 && id[len - 1] == 0) --len;
        bytes memory out = new bytes(len);
        for (uint256 i = 0; i < len; ++i) {
            out[i] = id[i];
        }
        return string(out);
    }

    function _buildAttributes(IIntexNFT1155.SeriesData memory data) private pure returns (string memory) {
        string memory head = string.concat(
            "[{\"trait_type\":\"Series State\",\"value\":\"",
            _stateString(data.state),
            "\"},",
            "{\"trait_type\":\"Emission Day\",\"value\":",
            Strings.toString(data.emissionDay),
            ",\"display_type\":\"number\"},",
            _currencyTrait("Issuance Currency", data.issuanceCurrency),
            ",",
            _currencyTrait("Reference Currency", data.referenceCurrency)
        );

        // Raw minor units stay in readData. These are display values.
        string memory economics = string.concat(
            ",{\"trait_type\":\"Entry Price\",\"value\":",
            _amountPlain(data.entryPriceMinor, PRICE_DECIMALS, PRICE_PRECISION),
            ",\"display_type\":\"number\"},",
            "{\"trait_type\":\"Call Price\",\"value\":",
            _amountPlain(data.callPriceMinor, PRICE_DECIMALS, PRICE_PRECISION),
            ",\"display_type\":\"number\"},",
            "{\"trait_type\":\"Promis Load\",\"value\":",
            Strings.toString(data.promisLoadMinor / SCALE_1E6),
            ",\"display_type\":\"number\"}"
        );

        string memory callRows = "";
        if (data.calledAt != 0) {
            callRows = string.concat(
                ",{\"trait_type\":\"Called At\",\"value\":",
                Strings.toString(data.calledAt),
                ",\"display_type\":\"date\"},",
                "{\"trait_type\":\"Call Deadline\",\"value\":",
                Strings.toString(uint256(data.calledAt) + data.callTrigger.callNoticePeriod),
                ",\"display_type\":\"date\"}"
            );
        }

        return string.concat(head, economics, callRows, "]");
    }

    /// @dev Alpha-3 for an ISO 4217 code, the bare number otherwise.
    function _currencyTrait(string memory name, uint16 code) private pure returns (string memory) {
        (bool found, bytes3 alpha) = Iso4217.alphaOf(code);
        string memory value = found
            ? string.concat("\"", string(abi.encodePacked(alpha)), "\"")
            : string.concat(Strings.toString(code), ",\"display_type\":\"number\"");
        return string.concat("{\"trait_type\":\"", name, "\",\"value\":", value, "}");
    }

    function _generateSVG(IIntexNFT1155.SeriesData memory data, string memory displayId)
        private
        pure
        returns (string memory)
    {
        (string memory statusText, string memory statusColor) = _stateInfo(data.state);
        return string.concat(_svgHeader(displayId, statusText, statusColor), _svgData(data), "</svg>");
    }

    function _stateInfo(IIntexNFT1155.IntexState state_) private pure returns (string memory, string memory) {
        if (state_ == IIntexNFT1155.IntexState.Issued) return ("ISSUED", "#2563eb");
        if (state_ == IIntexNFT1155.IntexState.Forfeited) return ("FORFEITED", "#6b7280");
        return ("CALLED", "#f97316");
    }

    function _svgHeader(string memory displayId, string memory statusText, string memory statusColor)
        private
        pure
        returns (string memory)
    {
        return string.concat(
            "<svg width=\"600\" height=\"600\" xmlns=\"http://www.w3.org/2000/svg\">",
            "<rect width=\"600\" height=\"600\" fill=\"#1a1a1a\" rx=\"20\"/>",
            "<rect x=\"15\" y=\"15\" width=\"570\" height=\"570\" fill=\"none\" stroke=\"#444\" stroke-width=\"2\" rx=\"15\"/>",
            "<text x=\"300\" y=\"70\" font-family=\"sans-serif\" font-size=\"32\" font-weight=\"bold\" fill=\"#fff\" text-anchor=\"middle\">INTEX</text>",
            "<text x=\"300\" y=\"110\" font-family=\"sans-serif\" font-size=\"24\" font-weight=\"600\" fill=\"#cbd5f5\" text-anchor=\"middle\">",
            displayId,
            "</text>",
            string.concat(
                "<rect x=\"200\" y=\"130\" width=\"200\" height=\"50\" fill=\"",
                statusColor,
                "\" rx=\"25\" opacity=\"0.3\"/>",
                "<text x=\"300\" y=\"163\" font-family=\"sans-serif\" font-size=\"22\" font-weight=\"bold\" fill=\"",
                statusColor,
                "\" text-anchor=\"middle\">",
                statusText,
                "</text>",
                "<line x1=\"60\" y1=\"220\" x2=\"540\" y2=\"220\" stroke=\"#444\" stroke-width=\"2\"/>"
            )
        );
    }

    function _svgData(IIntexNFT1155.SeriesData memory data) private pure returns (string memory) {
        uint256 y = ROW_TOP;
        string memory rows = _generateField("Promis Load", _formatInteger(data.promisLoadMinor / SCALE_1E6), y);
        y += ROW_STEP;
        rows = string.concat(
            rows, _generateField("Entry Price", _formatAmount(data.entryPriceMinor, PRICE_DECIMALS, PRICE_PRECISION), y)
        );
        y += ROW_STEP;
        rows = string.concat(
            rows, _generateField("Call Price", _formatAmount(data.callPriceMinor, PRICE_DECIMALS, PRICE_PRECISION), y)
        );
        y += ROW_STEP;
        if (data.calledAt != 0) {
            uint256 deadline = uint256(data.calledAt) + data.callTrigger.callNoticePeriod;
            rows = string.concat(rows, _generateField("Call Deadline", _formatTimestamp(deadline), y));
        }
        return rows;
    }

    function _generateField(string memory label, string memory value, uint256 yPos)
        private
        pure
        returns (string memory)
    {
        return string.concat(
            "<text x=\"60\" y=\"",
            Strings.toString(yPos),
            "\" font-family=\"sans-serif\" font-size=\"20\" fill=\"#999\">",
            label,
            "</text>",
            "<text x=\"540\" y=\"",
            Strings.toString(yPos),
            "\" font-family=\"sans-serif\" font-size=\"20\" fill=\"#fff\" text-anchor=\"end\">",
            value,
            "</text>"
        );
    }

    function _stateString(IIntexNFT1155.IntexState state_) private pure returns (string memory) {
        if (state_ == IIntexNFT1155.IntexState.Issued) return "Issued";
        if (state_ == IIntexNFT1155.IntexState.Forfeited) return "Forfeited";
        return "Called";
    }

    /// @dev Fixed-point amount without thousand separators - safe as a JSON number.
    function _amountPlain(uint256 amount, uint8 decimals, uint8 shown) private pure returns (string memory) {
        uint256 divisor = 10 ** decimals;
        return string.concat(Strings.toString(amount / divisor), _fraction(amount % divisor, decimals, shown));
    }

    /// @dev Fixed-point amount with thousand separators - SVG display only.
    function _formatAmount(uint256 amount, uint8 decimals, uint8 shown) private pure returns (string memory) {
        uint256 divisor = 10 ** decimals;
        return string.concat(_formatInteger(amount / divisor), _fraction(amount % divisor, decimals, shown));
    }

    /// @dev `.ddd` capped at `shown` digits, trailing zeros trimmed. Empty when nothing remains.
    ///      Leading zeros are kept, so 0.001 never collapses to 0.1.
    function _fraction(uint256 remainder, uint8 decimals, uint8 shown) private pure returns (string memory) {
        uint256 value = remainder / (10 ** (decimals - shown));
        if (value == 0) return "";

        bytes memory digits = bytes(Strings.toString(value));
        uint256 lead = shown - digits.length;
        uint256 kept = digits.length;
        while (digits[kept - 1] == "0") {
            --kept;
        }

        bytes memory out = new bytes(1 + lead + kept);
        out[0] = ".";
        for (uint256 i = 0; i < lead; ++i) {
            out[1 + i] = "0";
        }
        for (uint256 i = 0; i < kept; ++i) {
            out[1 + lead + i] = digits[i];
        }
        return string(out);
    }

    function _formatInteger(uint256 value) private pure returns (string memory) {
        if (value == 0) return "0";

        bytes memory valueBytes = bytes(Strings.toString(value));
        uint256 length = valueBytes.length;
        uint256 commas = (length - 1) / 3;
        bytes memory result = new bytes(length + commas);

        uint256 resultIndex = result.length;
        uint256 digitCount = 0;

        for (uint256 i = length; i > 0; --i) {
            if (digitCount == 3) {
                --resultIndex;
                result[resultIndex] = ",";
                digitCount = 0;
            }
            --resultIndex;
            result[resultIndex] = valueBytes[i - 1];
            ++digitCount;
        }

        return string(result);
    }

    /// @dev Unix timestamp as "DD.MM.YYYY HH:MM UTC".
    function _formatTimestamp(uint256 timestamp) private pure returns (string memory) {
        uint256 date = DateKey.fromTimestamp(timestamp);
        uint256 remainder = timestamp % 1 days;
        return string.concat(
            _pad2(date % 100),
            ".",
            _pad2((date / 100) % 100),
            ".",
            Strings.toString(date / 10_000),
            " ",
            _pad2(remainder / 1 hours),
            ":",
            _pad2((remainder % 1 hours) / 1 minutes),
            " UTC"
        );
    }

    function _pad2(uint256 n) private pure returns (string memory) {
        if (n < 10) return string.concat("0", Strings.toString(n));
        return Strings.toString(n);
    }
}
