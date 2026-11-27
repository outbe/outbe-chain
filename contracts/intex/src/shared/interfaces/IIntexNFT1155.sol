// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC1155} from "@openzeppelin/contracts/token/ERC1155/IERC1155.sol";
import {IERC1155Bridgeable} from "./IERC1155Bridgeable.sol";

/**
 * @title IntexNFT1155 Contract Interface
 * @author Outbe
 * @notice Public API, events, errors, and data types for `IntexNFT1155`.
 * @dev Series are keyed by `seriesId` (bytes14). A series' Issued units are the ERC1155 token id
 * `uint256(keccak256(seriesId))`, the series' `intexId`. Settled units live in the Intex
 * precompile on Outbe.
 * Also implements `IERC1155Bridgeable` for ERC-7786 cross-chain compatibility.
 * @dev Keep continuation lines flush against the leading `*`. The Rust
 * precompiles bind this interface with `sol!`, which re-emits this block as a
 * doc comment. A 4-space indent there parses as a Rust code block, and rustdoc
 * then compiles it as a doctest.
 */
interface IIntexNFT1155 is IERC1155, IERC1155Bridgeable {
    // Implementers inherit the following standard methods and make them available
    // (from OpenZeppelin IERC1155/ERC1155):
    // - balanceOf(address account, uint256 id) external view returns (uint256)
    // - balanceOfBatch(address[] calldata accounts, uint256[] calldata ids) external view returns (uint256[] memory)
    // - setApprovalForAll(address operator, bool approved) external
    // - isApprovedForAll(address account, address operator) external view returns (bool)
    // - safeTransferFrom(address from, address to, uint256 id, uint256 amount, bytes calldata data) external
    // - safeBatchTransferFrom(address from, address to, uint256[] calldata ids, uint256[] calldata amounts, bytes calldata data) external

    // --- Types ---

    /// @notice Series lifecycle state.
    /// @dev Issued -> Called -> Forfeited. `Forfeited` is read-only: storage keeps `Called`, so the
    ///      freezes that compare the stored field keep applying.
    enum IntexState {
        Issued,
        Called,
        Forfeited
    }

    /// @notice Forced-call trigger parameters (window/threshold/period).
    struct IntexCallTrigger {
        /// @notice Call-trigger observation window in seconds.
        uint32 callWindow;
        /// @notice Call-trigger threshold in seconds.
        uint32 callThreshold;
        /// @notice Called->deadline window in seconds. It is stored verbatim. The issuer must
        ///         supply a non-zero value.
        uint32 callNoticePeriod;
        /// @notice Seconds past the settlement deadline a Called unit may still arrive and settle.
        uint32 transitMargin;
    }

    /// @notice Series-level data, stored once per series under its Issued token id.
    struct SeriesData {
        /// @notice Issuance currency (ISO numeric). Single USD (840) until multi-currency.
        uint16 issuanceCurrency;
        /// @notice Reference currency (ISO numeric). Single USD (840) until multi-currency.
        uint16 referenceCurrency;
        /// @notice Auction-cleared units the series was issued with. Set once at `openSeries`,
        ///         never mutated.
        uint32 totalUnits;
        /// @notice PROMIS-units per Intex unit (1e6).
        uint128 promisLoadMinor;
        /// @notice Per-unit entry price in ISO stable-units (1e6).
        uint64 entryPriceMinor;
        /// @notice Call price in ISO stable-units (1e6).
        uint64 callPriceMinor;
        /// @notice Series identifier - the readable id this record belongs to.
        bytes14 seriesId;
        /// @notice Forced-call trigger (window/threshold/period).
        IntexCallTrigger callTrigger;
        /// @notice Timestamp when the series was created (UNIX seconds).
        uint64 issuedAt;
        /// @notice Timestamp when the series entered the Called state (UNIX seconds, 0 if not
        ///         called).
        uint64 calledAt;
        /// @notice Total supply of this token id across all owners.
        uint32 totalSupply;
        /// @notice Current series lifecycle state.
        IntexState state;
        /// @notice Emission day whose auction issued this series.
        uint32 emissionDay;
    }

    // --- Events ---

    /// @notice Emitted when Issued Intexes are minted to an owner.
    /// @param intexId Issued token id (= `keccak256(seriesId)`).
    /// @param owner Recipient of the minted Issued tokens.
    /// @param seriesId Series identifier.
    /// @param issuanceCurrency Issuance currency (ISO numeric).
    /// @param referenceCurrency Reference currency (ISO numeric).
    /// @param units Issued units minted to `owner`.
    /// @param payload ABI-encoded total Promis load (`abi.encode(uint256(promisLoadMinor) * units)`).
    event IntexIssued(
        uint256 indexed intexId,
        address indexed owner,
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint32 units,
        bytes payload
    );

    /// @notice Emitted when this chain creates a series from the terms that Outbe sent. It has the
    ///         shape of the Intex factory's `IntexSeriesOpened`.
    event IntexSeriesOpened(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 entryPriceMinor,
        uint256 callPriceMinor,
        uint64 callWindow,
        uint64 callThreshold,
        uint64 callNoticePeriod,
        uint64 transitMargin,
        uint32 units,
        uint256 unitPromisLoadMinor
    );

    /// @notice Emitted when a merchant's Issued units leave their series for a Gem Factory position.
    ///         `gemFactoryPositionId` links them to `GemFactoryPositionCreated` of the same transaction.
    event IntexSentToGemFactory(
        uint256 indexed intexId, address indexed owner, uint256 indexed gemFactoryPositionId, uint32 units
    );

    /// @notice Emitted when this chain records the call notice of a series.
    /// @param seriesId Series identifier.
    /// @param issuanceCurrency Issuance currency (ISO numeric).
    /// @param referenceCurrency Reference currency (ISO numeric).
    /// @param calledAt Unix time the origin marked the series Called.
    /// @param settlementDeadline `calledAt + callNoticePeriod`, capped at `uint64.max`.
    event IntexSeriesCalled(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint64 calledAt,
        uint64 settlementDeadline
    );

    /// @notice Emitted when an owner's units of a forfeited series are burned on this chain. The
    ///         value of these units was already returned at the close of the series on Outbe.
    event IntexForfeited(
        uint256 indexed intexId, address indexed owner, uint16 issuanceCurrency, uint16 referenceCurrency, uint32 units
    );

    /// @notice Emitted when token metadata is updated (ERC-4906). Per the EIP, `tokenId` is
    ///         non-indexed.
    /// @param tokenId Token id whose metadata changed.
    event MetadataUpdate(uint256 tokenId);

    // --- Errors ---

    /// @notice Zero address provided.
    error ZeroAddress(string field, address value);
    /// @notice Invalid lifecycle state transition.
    error InvalidState(uint8 expected, uint8 actual);
    /// @notice No Intex has this id.
    error IntexNotFound(uint256 intexId);
    /// @notice Series already exists for this token id.
    error IntexAlreadyExists(uint256 intexId);
    /// @notice `openSeries` was called with a zero issued-intex count.
    error ZeroTotalUnits();
    /// @notice `issuedAt` is zero (the existence sentinel) or dated after this chain's clock.
    error InvalidIssuedAt(uint64 issuedAt);
    /// @notice A burn of zero units.
    error ZeroUnits();
    /// @notice A mint or crosschainMint exceeds the units its packed storage field can hold.
    error UnitsTooLarge(uint256 units);
    /// @notice Owner-to-owner transfer attempted on a `Called` series after its settlement deadline.
    error TransferAfterDeadline(uint256 intexId, uint64 deadline);
    /// @notice Bridge attempted on a `Called` series after it closed: crosschainBurn after the
    ///         settlement deadline, crosschainMint after the transit margin past it.
    error BridgeAfterDeadline(uint256 intexId, uint64 closedAt);
    /// @notice Issue attempted on a `Called` series after the transit margin past its settlement
    ///         deadline ended.
    error IssueAfterDeadline(uint256 intexId, uint64 closedAt);
    /// @notice `callSeries` was given a call time of zero or one the destination clock has not
    ///         reached.
    error CalledAtInvalid(uint64 calledAt, uint64 nowTs);
    /// @notice An `issueIntex` or `crosschainMint` would push `totalSupply` past
    ///         `type(uint32).max`.
    error SupplyCapExceeded(bytes14 seriesId, uint256 attempted, uint256 cap);

    // --- Writes ---

    /// @notice Identity inputs for a new series, set once at `openSeries`.
    /// @dev `emissionDay` is the day the series was derived from, stored verbatim rather than
    ///      inferred from `seriesId`.
    struct OpenSeriesParams {
        bytes14 seriesId;
        uint32 emissionDay;
        /// @notice When the origin created the series. The call window is dated from it.
        uint64 issuedAt;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint32 totalUnits;
        uint128 promisLoadMinor;
        uint64 entryPriceMinor;
        uint64 callPriceMinor;
        IntexCallTrigger callTrigger;
    }

    /// @notice Create a new Intex series (one per auction) with its identity fields.
    /// @param params Series identity (id, currencies, issued units, promis load, prices, call
    ///        trigger).
    function openSeries(OpenSeriesParams calldata params) external;

    /// @notice Mint Intex to a specific address.
    /// @dev A `Called` series takes no issuance past the transit margin after its settlement deadline.
    /// @param to Recipient of the minted Issued tokens.
    /// @param units Units to mint, bounded by `type(uint16).max`. `totalSupply` stays within
    ///        `uint32`.
    /// @param seriesId Series identifier.
    function issueIntex(address to, uint256 units, bytes14 seriesId) external;

    /// @notice Mark a series as Called (Issued -> Called).
    /// @param seriesId Series identifier.
    /// @param calledAt Unix time the origin marked the series Called. The deadline derives from it.
    function callSeries(bytes14 seriesId, uint64 calledAt) external;

    /// @notice Settle `units` of `owner`'s Issued Intex on Outbe: they leave this token and become
    ///         Settled units of the Intex on Outbe.
    /// @dev Entry point of the IntexFactory precompile under BURNER_ROLE. It owns the series
    ///      state, so this burn checks none.
    /// @param owner Owner whose Issued tokens are burned.
    /// @param seriesId Series identifier.
    /// @param units Issued units to burn.
    function settleIntex(address owner, bytes14 seriesId, uint256 units) external;

    /// @notice Move `units` of `owner`'s Issued Intex on Outbe into the Gem Factory position
    ///         `gemFactoryPositionId`: the units leave the series and end with `IntexSentToGemFactory`.
    /// @dev Entry point of the GemFactory precompile under BURNER_ROLE. It owns the series state,
    ///      so this call checks none.
    function sendIntexToGemFactory(address owner, bytes14 seriesId, uint256 units, uint256 gemFactoryPositionId)
        external;

    /// @notice Burn all of `owner`'s units of a series that reads Forfeited on this chain. Any
    ///         account may call it. Reverts before the transit close or for an empty balance.
    /// @param owner Owner whose forfeited units are burned.
    /// @param seriesId Series identifier.
    function forfeitIntex(address owner, bytes14 seriesId) external;

    // --- Reads ---

    /// @notice Whether the series has been created here.
    /// @param seriesId Series identifier.
    /// @return True once `openSeries` has run for it.
    function seriesExists(bytes14 seriesId) external view returns (bool);

    /// @notice Issued token id for a series (= `uint256(keccak256(seriesId))`, its `intexId`). Pure helper.
    /// @param seriesId Series identifier.
    /// @return The Issued token id.
    function issuedIntexId(bytes14 seriesId) external pure returns (uint256);

    /// @notice Series a day's currency pair was created under.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param issuanceCurrency Issuance currency (ISO numeric).
    /// @param referenceCurrency Reference currency (ISO numeric).
    /// @return The series id, or zero if the pair has no series that day.
    function seriesIdOf(uint32 emissionDay, uint16 issuanceCurrency, uint16 referenceCurrency)
        external
        view
        returns (bytes14);

    /// @notice Read series data by series id. `state` derives Forfeited, as the card renders it.
    /// @param seriesId Series identifier.
    /// @return The full series data for the Issued token id.
    function getIntexSeries(bytes14 seriesId) external view returns (SeriesData memory);

    /// @notice Read series data by its token id, `keccak256(seriesId)`, as `getIntexSeries` does.
    /// @param intexId Issued token id.
    /// @return The full series data for the token id.
    function getIntex(uint256 intexId) external view returns (SeriesData memory);

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
}
