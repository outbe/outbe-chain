// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Strings} from "@openzeppelin/contracts/utils/Strings.sol";
import {Base64} from "@openzeppelin/contracts/utils/Base64.sol";
import {IIntexNFT1155} from "../interfaces/IIntexNFT1155.sol";

/// @title IntexMetadata
/// @notice JSON + SVG metadata rendering for IntexNFT1155 tokens.
/// @dev External linked library: deployed once and delegatecalled, keeping the renderer's
///      bytecode outside IntexNFT1155's EIP-170 budget. All functions are pure — the caller
///      passes a SeriesData snapshot plus the current timestamp (drives the derived Expired
///      display; expiry is not an on-chain state).
library IntexMetadata {
    string internal constant DESCRIPTION =
        "Intex is the core cross-chain asset of the Outbe network. Each series is born from a Worldwide Day auction held across all connected chains; its transferable Issued tokens settle into soulbound Settled tokens that mine Promis.";

    /// @notice Build the `data:application/json;base64,...` URI for a token.
    /// @param data Series record for the token id (Issued or Settled class).
    /// @param timestamp Current block timestamp.
    /// @return Token URI; the collection document when the record does not exist (`issuedAt == 0`).
    function tokenURI(IIntexNFT1155.SeriesData memory data, uint256 timestamp) external pure returns (string memory) {
        if (data.issuedAt == 0) return _collectionURI();

        bool settled = data.status == IIntexNFT1155.IntexStatus.Settled;
        bool expired = !settled && data.state == IIntexNFT1155.IntexState.Called
            && timestamp > uint256(data.calledAt) + data.callTrigger.intexCallPeriod;
        string memory displayId = _displayId(data);

        string memory json = string.concat(
            "{\"name\":\"Intex Series ",
            displayId,
            settled ? " - Settled" : "",
            "\",\"description\":\"",
            DESCRIPTION,
            "\",\"image\":\"data:image/svg+xml;base64,",
            Base64.encode(bytes(_generateSVG(data, displayId, settled, expired))),
            "\",\"attributes\":",
            _buildAttributes(data, settled, expired),
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
            Base64.encode(bytes(string.concat("{\"name\":\"Intex\",\"description\":\"", DESCRIPTION, "\"}")))
        );
    }

    /// @dev `<worldwideDay>-<issuance ccy>-<reference ccy>` — the composite series id format.
    function _displayId(IIntexNFT1155.SeriesData memory data) private pure returns (string memory) {
        return string.concat(
            Strings.toString(data.worldwideDay), "-", _pad3(data.issuanceCurrency), "-", _pad3(data.referenceCurrency)
        );
    }

    function _buildAttributes(IIntexNFT1155.SeriesData memory data, bool settled, bool expired)
        private
        pure
        returns (string memory)
    {
        string memory head = string.concat(
            "[{\"trait_type\":\"Token Status\",\"value\":\"",
            settled ? "Settled" : "Issued",
            "\"},",
            settled
                ? ""
                : string.concat(
                    "{\"trait_type\":\"Series State\",\"value\":\"", _stateString(data.state, expired), "\"},"
                ),
            "{\"trait_type\":\"Worldwide Day\",\"value\":",
            Strings.toString(data.worldwideDay),
            ",\"display_type\":\"number\"},",
            "{\"trait_type\":\"Issuance Currency\",\"value\":",
            Strings.toString(data.issuanceCurrency),
            ",\"display_type\":\"number\"},",
            "{\"trait_type\":\"Reference Currency\",\"value\":",
            Strings.toString(data.referenceCurrency),
            ",\"display_type\":\"number\"}"
        );

        // Prices and load are human-formatted JSON numbers (comma-free); raw values stay in readData.
        string memory economics = string.concat(
            ",{\"trait_type\":\"Entry Price\",\"value\":",
            _amountPlain(data.entryPriceMinor, 2),
            ",\"display_type\":\"number\"},",
            "{\"trait_type\":\"Floor Price\",\"value\":",
            _amountPlain(data.floorPriceMinor, 2),
            ",\"display_type\":\"number\"},",
            "{\"trait_type\":\"Call Price\",\"value\":",
            _amountPlain(data.callPriceMinor, 2),
            ",\"display_type\":\"number\"},",
            "{\"trait_type\":\"Promis Load\",\"value\":",
            Strings.toString(data.promisLoadMinor / 1e18),
            ",\"display_type\":\"number\"}"
        );

        string memory callRows = "";
        if (!settled && data.calledAt != 0) {
            callRows = string.concat(
                ",{\"trait_type\":\"Called At\",\"value\":",
                Strings.toString(data.calledAt),
                ",\"display_type\":\"date\"},",
                "{\"trait_type\":\"Call Deadline\",\"value\":",
                Strings.toString(uint256(data.calledAt) + data.callTrigger.intexCallPeriod),
                ",\"display_type\":\"date\"}"
            );
        }

        return string.concat(head, economics, callRows, "]");
    }

    function _generateSVG(IIntexNFT1155.SeriesData memory data, string memory displayId, bool settled, bool expired)
        private
        pure
        returns (string memory)
    {
        string memory statusText;
        string memory statusColor;
        if (settled) {
            (statusText, statusColor) = ("SETTLED", "#a855f7");
        } else if (expired) {
            (statusText, statusColor) = ("EXPIRED", "#6b7280");
        } else {
            (statusText, statusColor) = _stateInfo(data.state);
        }

        return string.concat(_svgHeader(displayId, statusText, statusColor), _svgData(data, settled), "</svg>");
    }

    function _stateInfo(IIntexNFT1155.IntexState state_) private pure returns (string memory, string memory) {
        if (state_ == IIntexNFT1155.IntexState.Issued) return ("ISSUED", "#2563eb");
        if (state_ == IIntexNFT1155.IntexState.Qualified) return ("QUALIFIED", "#16a34a");
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
            "<text x=\"300\" y=\"70\" font-family=\"sans-serif\" font-size=\"32\" font-weight=\"bold\" fill=\"#fff\" text-anchor=\"middle\">INTEX SERIES</text>",
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

    function _svgData(IIntexNFT1155.SeriesData memory data, bool settled) private pure returns (string memory) {
        string memory rows = string.concat(
            _generateField("Entry Price", _formatAmount(data.entryPriceMinor, 2), 265),
            _generateField("Floor Price", _formatAmount(data.floorPriceMinor, 2), 310),
            _generateField("Call Price", _formatAmount(data.callPriceMinor, 2), 355),
            _generateField("Promis Load", _formatInteger(data.promisLoadMinor / 1e18), 400)
        );
        if (!settled && data.calledAt != 0) {
            uint256 deadline = uint256(data.calledAt) + data.callTrigger.intexCallPeriod;
            rows = string.concat(rows, _generateField("Call Deadline", _formatTimestamp(deadline), 445));
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

    function _stateString(IIntexNFT1155.IntexState state_, bool expired) private pure returns (string memory) {
        if (expired) return "Expired";
        if (state_ == IIntexNFT1155.IntexState.Issued) return "Issued";
        if (state_ == IIntexNFT1155.IntexState.Qualified) return "Qualified";
        return "Called";
    }

    /// @dev Fixed-point amount as `int.dd` without thousand separators — safe as a JSON number.
    function _amountPlain(uint256 amount, uint8 decimals) private pure returns (string memory) {
        uint256 divisor = 10 ** decimals;
        uint256 decimalPart = (amount % divisor) / (10 ** (decimals - 2));
        return string.concat(
            Strings.toString(amount / divisor), ".", decimalPart < 10 ? "0" : "", Strings.toString(decimalPart)
        );
    }

    /// @dev Fixed-point amount as `int.dd` with thousand separators — SVG display only.
    function _formatAmount(uint256 amount, uint8 decimals) private pure returns (string memory) {
        uint256 divisor = 10 ** decimals;
        uint256 decimalPart = (amount % divisor) / (10 ** (decimals - 2));
        return string.concat(
            _formatInteger(amount / divisor), ".", decimalPart < 10 ? "0" : "", Strings.toString(decimalPart)
        );
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

    /// @dev Unix timestamp as "DD.MM.YYYY HH:MM UTC" via Howard Hinnant's civil_from_days —
    ///      divide-then-multiply is exact integer date math, not precision loss.
    // slither-disable-start divide-before-multiply
    function _formatTimestamp(uint256 timestamp) private pure returns (string memory) {
        uint256 z = timestamp / 86400 + 719468;
        uint256 era = z / 146097;
        uint256 doe = z - era * 146097;
        uint256 yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        uint256 y = yoe + era * 400;
        uint256 doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        uint256 mp = (5 * doy + 2) / 153;
        uint256 d = doy - (153 * mp + 2) / 5 + 1;
        uint256 m = mp < 10 ? mp + 3 : mp - 9;
        if (m <= 2) y += 1;

        uint256 remainder = timestamp % 86400;
        uint256 h = remainder / 3600;
        uint256 mi = (remainder % 3600) / 60;

        return string.concat(_pad2(d), ".", _pad2(m), ".", Strings.toString(y), " ", _pad2(h), ":", _pad2(mi), " UTC");
    }
    // slither-disable-end divide-before-multiply

    function _pad2(uint256 n) private pure returns (string memory) {
        if (n < 10) return string.concat("0", Strings.toString(n));
        return Strings.toString(n);
    }

    /// @dev ISO 4217 numeric codes are three digits, zero-padded.
    function _pad3(uint16 n) private pure returns (string memory) {
        if (n < 10) return string.concat("00", Strings.toString(n));
        if (n < 100) return string.concat("0", Strings.toString(n));
        return Strings.toString(n);
    }
}
