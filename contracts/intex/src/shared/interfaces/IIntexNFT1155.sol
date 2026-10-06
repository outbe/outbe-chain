// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC1155} from "@openzeppelin/contracts/token/ERC1155/IERC1155.sol";
import {IERC1155Bridgeable} from "./IERC1155Bridgeable.sol";

/**
 * @title IntexNFT1155 Contract Interface
 * @author Outbe
 * @notice Public API, events, errors, and data types for `IntexNFT1155`.
 * @dev Series are keyed by `seriesId` (bytes14). Each series has two ERC1155 token ids:
 * issued = `uint112(seriesId)`, settled = the same with bit 112 set.
 * Also implements `IERC1155Bridgeable` for ERC-7786 cross-chain compatibility.
 * @dev Keep continuation lines flush against the leading `*`. The Rust
 * precompiles bind this interface with `sol!`, which re-emits this block as a
 * doc comment; a 4-space indent there parses as a Rust code block and is then
 * compiled as a doctest.
 */
interface IIntexNFT1155 is IERC1155, IERC1155Bridgeable {
    // The following standard methods are inherited and available on implementers (from OpenZeppelin IERC1155/ERC1155):
    // - balanceOf(address account, uint256 id) external view returns (uint256)
    // - balanceOfBatch(address[] calldata accounts, uint256[] calldata ids) external view returns (uint256[] memory)
    // - setApprovalForAll(address operator, bool approved) external
    // - isApprovedForAll(address account, address operator) external view returns (bool)
    // - safeTransferFrom(address from, address to, uint256 id, uint256 amount, bytes calldata data) external
    // - safeBatchTransferFrom(address from, address to, uint256[] calldata ids, uint256[] calldata amounts, bytes calldata data) external

    // --- Types ---

    /// @notice Series lifecycle state.
    /// @dev Issued -> Called -> Expired; `Qualified` is derived, never stored. `Expired` is read-only:
    ///      storage keeps `Called`, so the freezes that compare the stored field keep applying.
    enum IntexState {
        Issued,
        Qualified,
        Called,
        Expired
    }

    /// @notice Per-token classification within a series.
    /// @dev Each series has an Issued token (transferable, gated by series state) and a
    ///      Settled token (soulbound, minted on settle, burned on Promis mining).
    enum IntexStatus {
        Issued,
        Settled
    }

    /// @notice Per-owner, per-series balance pair. Widths match the `uint32` supply counters so a
    ///         balance accumulated above `type(uint16).max` is reported without truncation.
    struct OwnerBalances {
        uint32 issuedUnits;
        uint32 settledUnits;
    }

    /// @notice Forced-call trigger parameters (window/threshold/period).
    struct IntexCallTrigger {
        /// @notice Call-trigger observation window in seconds.
        uint32 callWindow;
        /// @notice Call-trigger threshold in seconds.
        uint32 callThreshold;
        /// @notice Called->deadline window in seconds; stored verbatim, the issuer must supply a non-zero value.
        uint32 callNoticePeriod;
    }

    /// @notice Series-level data, stored once per series under its Issued token id.
    struct SeriesData {
        /// @notice Issuance currency (ISO numeric); single USD (840) until multi-currency.
        uint16 issuanceCurrency;
        /// @notice Reference currency (ISO numeric); single USD (840) until multi-currency.
        uint16 referenceCurrency;
        /// @notice Auction-cleared units the series was issued with. Set once at `createSeries`, never mutated.
        uint32 issuedUnits;
        /// @notice PROMIS-units per Intex unit (1e6).
        uint128 promisLoadMinor;
        /// @notice Per-unit entry price in ISO stable-units (1e6).
        uint64 entryPriceMinor;
        /// @notice Floor price in ISO stable-units (1e6).
        uint64 floorPriceMinor;
        /// @notice Call price in ISO stable-units (1e6).
        uint64 callPriceMinor;
        /// @notice Forced-call trigger (window/threshold/period).
        IntexCallTrigger callTrigger;
        /// @notice Timestamp when the series was created (UNIX seconds).
        uint32 issuedAt;
        /// @notice Timestamp when the series entered the Called state (UNIX seconds, 0 if not called).
        uint32 calledAt;
        /// @notice Total supply of this token id across all owners.
        uint32 totalSupply;
        /// @notice Current series lifecycle state.
        IntexState state;
        /// @notice Worldwide day whose tributes fed this series.
        uint32 worldwideDay;
        /// @notice Series identifier - the readable id this record belongs to.
        bytes14 seriesId;
    }

    // --- Events ---

    /// @notice Emitted when a new Intex series is issued.
    /// @param operator Caller that minted the Issued tokens (`RELAYER_ROLE`).
    /// @param tokenId Issued token id (= `uint256(seriesId)`).
    /// @param to Recipient of the minted Issued tokens.
    /// @param units Issued units minted to `to`.
    event IntexIssued(address indexed operator, uint256 indexed tokenId, address indexed to, uint256 units);

    /// @notice Emitted when a series lifecycle state changes.
    /// @param operator Caller that drove the transition (`RELAYER_ROLE`).
    /// @param tokenId Issued token id (= `uint256(seriesId)`).
    /// @param fromState Lifecycle state before the transition.
    /// @param toState Lifecycle state after the transition.
    /// @param at Timestamp of the state change.
    /// @param settlementDeadline Effective settlement deadline (`calledAt + callNoticePeriod`, capped at `uint32.max`;
    ///        0 if not applicable).
    event IntexStatusUpdated(
        address indexed operator,
        uint256 indexed tokenId,
        IntexState fromState,
        IntexState toState,
        uint32 at,
        uint32 settlementDeadline
    );

    /// @notice Emitted when token metadata is updated (ERC-4906; `tokenId` is non-indexed per the EIP).
    /// @param tokenId Token id whose metadata changed.
    event MetadataUpdate(uint256 tokenId);

    /// @notice Emitted when a settled Intex right is exercised: its units burn to mine Promis.
    /// @param seriesId Series identifier.
    /// @param owner Owner whose Settled tokens were burned.
    /// @param units Settled units burned.
    event IntexExercised(bytes14 indexed seriesId, address indexed owner, uint256 units);

    event VwapSourceSet(address indexed source);

    /// @notice Emitted when Issued Intex are burned on being sent to the Gem Factory.
    /// @param seriesId Series identifier.
    /// @param owner Owner whose Issued tokens were burned.
    /// @param units Issued units burned.
    event IntexSentToGemFactory(bytes14 indexed seriesId, address indexed owner, uint256 units);

    // --- Errors ---

    /// @notice Zero address provided.
    error ZeroAddress(string field, address value);
    /// @notice Invalid lifecycle state transition.
    error InvalidState(uint8 expected, uint8 actual);
    /// @notice Token id does not exist.
    error NonexistentToken(uint256 tokenId);
    /// @notice Series already exists for this token id.
    error TokenAlreadyExists(uint256 tokenId);
    /// @notice `createSeries` was called with a zero issued-intex count.
    error ZeroIssuedUnits();
    /// @notice `issuedAt` is zero (the existence sentinel) or dated after this chain's clock.
    error InvalidIssuedAt(uint32 issuedAt);
    /// @notice A settlement or burn of zero units.
    error ZeroUnits();
    /// @notice A mint or crosschainMint exceeds the units its packed storage field can hold.
    error UnitsTooLarge(uint256 units);
    /// @notice Transfer or bridge attempted on a Settled (soulbound) token.
    error SoulboundSettled(uint256 tokenId);
    /// @notice Owner-to-owner transfer attempted while the series is Called.
    error TransferOnCalledForbidden(uint256 tokenId);
    /// @notice Bridge crosschainBurn/crosschainMint attempted on a Settled token.
    error BridgeOnSettledForbidden(uint256 tokenId);
    /// @notice Bridge crosschainBurn/crosschainMint attempted on a `Called` series after the settlement
    ///         deadline (`calledAt + callNoticePeriod`) has passed.
    error BridgeAfterDeadline(uint256 tokenId, uint32 deadline);
    /// @notice Settle attempted on a `Called` series after the settlement deadline
    ///         (`calledAt + callNoticePeriod`) has passed.
    error SettleAfterDeadline(uint256 tokenId, uint32 deadline);
    /// @notice Issue attempted on a `Called` series after the settlement deadline
    ///         (`calledAt + callNoticePeriod`) has passed.
    error IssueAfterDeadline(uint256 tokenId, uint32 deadline);
    /// @notice `markCalled` was given a call time of zero or one the destination clock has not reached.
    error CalledAtInvalid(uint32 calledAt, uint32 nowTs);
    /// @notice An `issueIntex` or `crosschainMint` would push `totalSupply` past `type(uint32).max`.
    error SupplyCapExceeded(bytes14 seriesId, uint256 attempted, uint256 cap);

    // --- Writes ---

    /// @notice Identity inputs for a new series, set once at `createSeries`.
    /// @dev `worldwideDay` is the day the series was derived from; it is the provenance key
    ///      (`seriesOfDay`), stored verbatim rather than inferred from `seriesId`.
    struct CreateSeriesParams {
        bytes14 seriesId;
        uint32 worldwideDay;
        /// @notice When the origin created the series; the call window is dated from it.
        uint32 issuedAt;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint32 issuedUnits;
        uint128 promisLoadMinor;
        uint64 entryPriceMinor;
        uint64 floorPriceMinor;
        uint64 callPriceMinor;
        IntexCallTrigger callTrigger;
    }

    /// @notice Create a new Intex series (one per auction) with its identity fields.
    /// @param params Series identity (id, currencies, issued units, promis load, prices, call trigger).
    function createSeries(CreateSeriesParams calldata params) external;

    /// @notice Mint Intex to a specific address.
    /// @dev A `Called` series takes no issuance past its settlement deadline.
    /// @param to Recipient of the minted Issued tokens.
    /// @param units Units to mint (bounded by `type(uint16).max`; `totalSupply` stays within `uint32`).
    /// @param seriesId Series identifier.
    function issueIntex(address to, uint256 units, bytes14 seriesId) external;

    /// @notice Mark a series as Called (Issued -> Called).
    /// @param seriesId Series identifier.
    /// @param calledAt Unix time the origin marked the series Called; the deadline derives from it.
    function markCalled(bytes14 seriesId, uint32 calledAt) external;

    /// @notice Burn `units` of `owner`'s Issued Intex and mint the same `units` of Settled Intex to `owner`.
    /// @dev Settlement-contract entry point under SETTLEMENT_ROLE. The caller checks qualification; a Called
    ///      series settles only until its deadline.
    /// @param seriesId Series identifier.
    /// @param owner Owner whose Issued tokens are burned and who receives the Settled tokens.
    /// @param units Issued units burned and Settled units minted.
    function settleIntex(bytes14 seriesId, address owner, uint256 units) external;

    /// @notice Burn `units` Settled Intex from `owner`.
    /// @dev Promis-facade entry point under PROMIS_ROLE.
    /// @param owner Owner whose Settled tokens are burned.
    /// @param seriesId Series identifier.
    /// @param units Settled units to burn.
    function burnSettled(address owner, bytes14 seriesId, uint256 units) external;

    /// @notice Burn `units` Issued Intex from `owner` when the tokens are sent to the Gem Factory.
    /// @dev Gem-factory entry point under GEM_ROLE. Only allowed while the series is tradable
    ///      (Issued - no Call Event yet). The capacity record lives in the
    ///      Gem Factory; the burned Intex is thereby non-tradable, call-exempt and Outbe-only.
    /// @param owner Owner whose Issued tokens are burned.
    /// @param seriesId Series identifier.
    /// @param units Issued units to burn.
    /// @return The units burned.
    function sendToGemFactory(address owner, bytes14 seriesId, uint256 units) external returns (uint256);

    /// @notice Zero derives no qualification.
    function setVwapSource(address source) external;

    // --- Reads ---

    function vwapSource() external view returns (address);

    /// @notice Whether the series has been created here.
    /// @param seriesId Series identifier.
    /// @return True once `createSeries` has run for it.
    function seriesExists(bytes14 seriesId) external view returns (bool);

    /// @notice Issued token id for a series (= `uint256(uint112(seriesId))`). Pure helper.
    /// @param seriesId Series identifier.
    /// @return The Issued token id.
    /// @notice Whether a finalized daily VWAP from the series' first full UTC day on closed above its floor,
    ///         as the card renders it. A missing or failing source reads as not qualified.
    /// @param seriesId Series identifier.
    function isQualified(bytes14 seriesId) external view returns (bool);

    function issuedTokenId(bytes14 seriesId) external pure returns (uint256);

    /// @notice Settled (soulbound) token id for a series (= the series id with bit 112 set). Pure helper.
    /// @param seriesId Series identifier.
    /// @return The Settled token id.
    function settledTokenId(bytes14 seriesId) external pure returns (uint256);

    /// @notice Worldwide day whose tributes fed the series (0 if the series does not exist).
    /// @param seriesId Series identifier.
    /// @return The worldwide day (yyyymmdd).
    function worldwideDayOf(bytes14 seriesId) external view returns (uint32);

    /// @notice Series ids issued for a worldwide day.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @return The series ids of that day.
    function seriesIdsByWorldwideDay(uint32 worldwideDay) external view returns (bytes14[] memory);

    /// @notice Both token ids for a series in one call.
    /// @param seriesId Series identifier.
    /// @return issued The Issued token id.
    /// @return settled The Settled token id.
    function tokenIds(bytes14 seriesId) external pure returns (uint256 issued, uint256 settled);

    /// @notice Token classification (Issued/Settled) for a token id, read off the id itself.
    /// @param tokenId Token id to classify.
    /// @return The token classification.
    function statusOf(uint256 tokenId) external pure returns (IntexStatus);

    /// @notice Read series data by series id; `state` derives Expired but not Qualified (see `isQualified`).
    /// @param seriesId Series identifier.
    /// @return The full series data for the Issued token id.
    function readData(bytes14 seriesId) external view returns (SeriesData memory);

    /// @notice Issued and Settled balances for an owner in a given series: this chain's raw
    ///         ERC-1155 balances, with no expiry applied and no other chain counted.
    /// @param seriesId Series identifier.
    /// @param owner Owner address to read.
    /// @return The owner's Issued and Settled balance pair.
    function ownerBalances(bytes14 seriesId, address owner) external view returns (OwnerBalances memory);

    /// @notice Total supply for a specific token id.
    /// @param tokenId Token id to read.
    /// @return The total supply of that token id across all owners.
    function totalSupply(uint256 tokenId) external view returns (uint256);

    /// @notice Token URI with on-chain metadata.
    /// @param tokenId Token id to render.
    /// @return The token URI containing on-chain metadata.
    function uri(uint256 tokenId) external view returns (string memory);

    /// @notice Collection name wallets and marketplaces display.
    /// @return The collection name.
    function name() external view returns (string memory);

    /// @notice Collection symbol wallets and marketplaces display.
    /// @return The collection symbol.
    function symbol() external view returns (string memory);

    /// @notice Collection-level metadata as an on-chain JSON data URI (ERC-7572).
    /// @return The collection metadata URI.
    function contractURI() external view returns (string memory);

    // --- Series reads ---

    /// @notice All series (token ids) that have been created.
    /// @return The Issued token ids of every created series.
    function getAllSeries() external view returns (uint256[] memory);

    /// @notice Series with pagination.
    /// @param offset Index into the full series array.
    /// @param limit Maximum slice length to return.
    /// @return series The requested slice of Issued token ids.
    /// @return total Total number of series created.
    function getSeriesPaginated(uint256 offset, uint256 limit)
        external
        view
        returns (uint256[] memory series, uint256 total);

    /// @notice Total number of series created.
    /// @return The count of created series.
    function totalSeries() external view returns (uint256);
}
