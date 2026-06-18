// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC1155} from "@openzeppelin/contracts/token/ERC1155/IERC1155.sol";
import {IERC1155Bridgeable} from "./IERC1155Bridgeable.sol";

/**
 * @title IntexNFT1155 Contract Interface
 * @author Outbe
 * @notice Public API, events, errors, and data types for `IntexNFT1155`.
 * @dev Series are keyed by `seriesId` (uint32). Each series has two ERC1155 token ids:
 *      issued = `uint256(seriesId)`, settled = `keccak256("SETTLED", seriesId)`.
 *      Also implements `IERC1155Bridgeable` for LayerZero cross-chain compatibility.
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
    /// @dev Lifecycle: Issued -> Qualified -> Called. Expiration after the call deadline
    ///      is signalled by the `SeriesExpired` event; it is not a distinct on-chain state.
    enum IntexState {
        Issued,
        Qualified,
        Called
    }

    /// @notice Per-token classification within a series.
    /// @dev Each series has an Issued token (transferable, gated by series state) and a
    ///      Settled token (soulbound, minted on settle, burned on Promis mining).
    enum IntexStatus {
        Issued,
        Settled
    }

    /// @notice Per-holder, per-series balance pair.
    struct HolderBalances {
        uint16 issued;
        uint16 settled;
    }

    /// @notice Series-level data, stored per token id (one entry for the Issued token id
    ///         and one for the Settled token id; `status` distinguishes them).
    /// @dev `issuedIntexCount` is meaningful only on the Issued entry; it caps the
    ///      cumulative `totalSupply` minted via `mint`/`mintBatch`.
    /// @dev Identity fields (promisLoadMinor, costAmountMinor, floorPriceMinor, etc.) live in
    ///      Intex; this struct holds only balance/lifecycle state.
    struct SeriesData {
        /// @notice Timestamp when the series was created (UNIX seconds).
        uint32 issuedAt;
        /// @notice Timestamp when the series entered the Called state (UNIX seconds, 0 if not called).
        uint32 calledAt;
        /// @notice Duration in seconds between `calledAt` and the settlement deadline.
        uint32 intexCallPeriod;
        /// @notice Total supply of this token id across all holders.
        uint32 totalSupply;
        /// @notice Auction-cleared cap on the cumulative Issued mint quantity. Set once at
        ///         `createSeries` and never mutated. `mint`/`mintBatch` reject anything that
        ///         would push `mintedCount` past this value.
        uint32 issuedIntexCount;
        /// @notice Monotonic count of Issued tokens ever minted via `mint`/`mintBatch`. Never
        ///         decremented — burns (`crosschainBurn`, `settle`, `expireSeries`) leave this field
        ///         untouched so a burn-then-remint cycle cannot reopen cap room.
        uint32 mintedCount;
        /// @notice Token classification (Issued or Settled).
        IntexStatus status;
        /// @notice Current series lifecycle state.
        IntexState state;
    }

    // --- Events ---

    /// @notice Emitted when a new Intex series is issued.
    /// @param operator Caller that minted the Issued tokens (`RELAYER_ROLE`).
    /// @param tokenId Issued token id (= `uint256(seriesId)`).
    /// @param to Recipient of the minted Issued tokens.
    /// @param quantity Amount of Issued tokens minted to `to`.
    event IntexIssued(address indexed operator, uint256 indexed tokenId, address indexed to, uint256 quantity);

    /// @notice Emitted when a series lifecycle state changes.
    /// @param operator Caller that drove the transition (`RELAYER_ROLE`).
    /// @param tokenId Issued token id (= `uint256(seriesId)`).
    /// @param fromState Lifecycle state before the transition.
    /// @param toState Lifecycle state after the transition.
    /// @param at Timestamp of the state change.
    /// @param callDeadlineAt Effective settlement deadline (`calledAt + intexCallPeriod`, 0 if not applicable).
    event IntexStatusUpdated(
        address indexed operator,
        uint256 indexed tokenId,
        IntexState fromState,
        IntexState toState,
        uint32 at,
        uint32 callDeadlineAt
    );

    /// @notice Emitted when token metadata is updated (ERC-4906).
    /// @param tokenId Token id whose metadata changed.
    event MetadataUpdate(uint256 indexed tokenId);

    /// @notice Emitted when collection metadata is updated.
    /// @param description New collection-level description string.
    event CollectionMetadataUpdated(string description);

    /// @notice Emitted when a series passes its call deadline without full settlement.
    /// @dev Fires once, on the page that drains the final Issued holder. Mid-page progress
    ///      is reported separately via `SeriesExpiredProgress`.
    /// @param tokenId Issued token id.
    /// @param account Address that triggered the expiration call.
    event SeriesExpired(uint256 indexed tokenId, address indexed account);

    /// @notice Emitted on every paginated `expireSeries` call that does not fully drain the
    ///         remaining holder set. Lets indexers track sweep progress without scanning logs
    ///         on the Issued token id.
    /// @param seriesId Series identifier.
    /// @param processed Number of holders swept in this call.
    event SeriesExpiredProgress(uint32 indexed seriesId, uint256 processed);

    /// @notice Emitted when settlement burns Issued and mints Settled.
    /// @param seriesId Series identifier.
    /// @param to Recipient of the newly minted Settled tokens.
    /// @param amount Amount of Issued burned and Settled minted.
    event IntexSettled(uint32 indexed seriesId, address indexed to, uint256 amount);

    /// @notice Emitted when Settled Intex are consumed to mine Promis.
    /// @param seriesId Series identifier.
    /// @param holder Holder whose Settled tokens were burned.
    /// @param amount Amount of Settled tokens burned.
    event IntexCompleted(uint32 indexed seriesId, address indexed holder, uint256 amount);

    // --- Errors ---

    /// @notice Zero address provided.
    error ZeroAddress(string field, address value);
    /// @notice Invalid lifecycle state transition.
    error InvalidState(uint8 expected, uint8 actual);
    /// @notice Token id does not exist.
    error NonexistentToken(uint256 tokenId);
    /// @notice `expireSeries` was called before the series passed its call deadline (or was never called).
    error SeriesNotYetExpired(uint32 deadline, uint32 nowTs);
    /// @notice `expireSeries` has nothing to expire — the series supply is already zero (swept).
    error NothingToExpire();
    /// @notice Provided call period is invalid (zero or out of allowed range).
    error InvalidCallPeriod(uint32 intexCallPeriod);
    /// @notice Series already exists for this token id.
    error TokenAlreadyExists(uint256 tokenId);
    /// @notice Input array lengths do not match.
    error ArrayLengthMismatch(uint256 length1, uint256 length2);
    /// @notice Empty array provided.
    error EmptyArray();
    /// @notice `createSeries` was called with a zero issued-intex count (the supply cap cannot be zero).
    error ZeroIssuedIntexCount();
    /// @notice A settlement or burn amount was zero.
    error ZeroAmount();
    /// @notice A mint or crosschainMint quantity exceeds the range its packed storage field can hold.
    error QuantityTooLarge(uint256 quantity);
    /// @notice Settle attempted in a series state that does not allow it.
    error InvalidStateForSettle(uint8 state);
    /// @notice Transfer or bridge attempted on a Settled (soulbound) token.
    error SoulboundSettled(uint256 tokenId);
    /// @notice Bridge crosschainBurn/crosschainMint attempted on a Settled token.
    error BridgeOnSettledForbidden(uint256 tokenId);
    /// @notice Bridge crosschainBurn/crosschainMint attempted while the series state disallows it.
    error BridgeStateForbidden(uint256 tokenId, uint8 state);
    /// @notice Bridge crosschainBurn/crosschainMint attempted on a `Called` series after the settlement
    ///         deadline (`calledAt + intexCallPeriod`) has passed.
    error BridgeAfterDeadline(uint256 tokenId, uint32 deadline);
    /// @notice Settle attempted on a `Called` series after the settlement deadline
    ///         (`calledAt + intexCallPeriod`) has passed.
    error SettleAfterDeadline(uint256 tokenId, uint32 deadline);
    /// @notice Read attempted on an unknown series id.
    error UnknownSeriesId(uint32 seriesId);
    /// @notice A mint or batch sum would push `totalSupply` past `issuedIntexCount`.
    error SupplyCapExceeded(uint32 seriesId, uint256 attempted, uint256 cap);
    /// @notice Pagination was invoked with a zero page limit (`expireSeries`,
    ///         `getIssuedHoldersWithBalances`).
    error ZeroLimit();
    /// @notice `collectionDescription` exceeds `MAX_COLLECTION_DESCRIPTION_BYTES`.
    error CollectionDescriptionTooLong();

    // --- Writes ---

    /// @notice Create a new Intex series (one per auction).
    /// @param seriesId Series identifier (yyyymmdd as uint32).
    /// @param issuedIntexCount Auction-cleared cap on the cumulative Issued mint quantity (must be > 0).
    /// @param intexCallPeriod Duration in seconds between Called and the settlement deadline (0 = default).
    function createSeries(uint32 seriesId, uint32 issuedIntexCount, uint32 intexCallPeriod) external;

    /// @notice Mint Intex to a specific address.
    /// @param to Recipient of the minted Issued tokens.
    /// @param quantity Amount to mint (bounded by `type(uint16).max` and the series supply cap).
    /// @param seriesId Series identifier.
    function mint(address to, uint256 quantity, uint32 seriesId) external;

    /// @notice Mint Intex to multiple addresses in one transaction.
    /// @param recipients Recipient addresses, parallel to `quantities`.
    /// @param quantities Per-recipient mint amounts, parallel to `recipients`.
    /// @param seriesId Series identifier.
    function mintBatch(address[] calldata recipients, uint256[] calldata quantities, uint32 seriesId) external;

    /// @notice Mark a series as Qualified (Issued -> Qualified).
    /// @param seriesId Series identifier.
    function markQualified(uint32 seriesId) external;

    /// @notice Mark a series as Called (Issued/Qualified -> Called).
    /// @param seriesId Series identifier.
    function markCalled(uint32 seriesId) external;

    /// @notice Signal that a Called series has passed its settlement deadline without full settlement.
    /// @dev Gated by `RELAYER_ROLE` — mass-burning balances is a privileged operation and
    ///      mirrors the role the bridge already uses for `markCalled` / `markQualified`.
    ///      Paginated to avoid block-gas-limit DoS on large holder sets: each call burns up
    ///      to `limit` of the remaining holders. Mid-page calls emit `SeriesExpiredProgress`;
    ///      the call that drains the last holder emits `SeriesExpired`. Once swept, the
    ///      function reverts because `totalSupply == 0` (idempotency invariant). `limit`
    ///      must be > 0.
    /// @param seriesId Series identifier.
    /// @param limit Maximum number of holders to sweep in this call.
    function expireSeries(uint32 seriesId, uint256 limit) external;

    /// @notice Set the collection metadata description.
    /// @param description New collection-level description (bounded by `MAX_COLLECTION_DESCRIPTION_BYTES`).
    function setCollectionMetadata(string calldata description) external;

    /// @notice Burn `amount` Issued Intex from `from` and mint the same `amount` of Settled Intex to `to`.
    /// @dev Settlement-contract entry point under SETTLEMENT_ROLE. Series must be Qualified or Called.
    /// @param seriesId Series identifier.
    /// @param from Holder whose Issued tokens are burned.
    /// @param to Recipient of the newly minted Settled tokens.
    /// @param amount Amount of Issued burned and Settled minted.
    function settle(uint32 seriesId, address from, address to, uint256 amount) external;

    /// @notice Burn `amount` Settled Intex from `holder`.
    /// @dev Promis-facade entry point under PROMIS_ROLE.
    /// @param holder Holder whose Settled tokens are burned.
    /// @param seriesId Series identifier.
    /// @param amount Amount of Settled tokens to burn.
    function burnSettled(address holder, uint32 seriesId, uint256 amount) external;

    // --- Reads ---

    /// @notice Upper bound on a series call period, in seconds. Implemented by the
    ///         `MAX_INTEX_CALL_PERIOD` public constant on the implementation.
    /// @return The maximum allowed call period in seconds.
    function MAX_INTEX_CALL_PERIOD() external view returns (uint32);

    /// @notice Issued token id for a series (= `uint256(seriesId)`). Pure helper.
    /// @param seriesId Series identifier.
    /// @return The Issued token id.
    function issuedTokenId(uint32 seriesId) external pure returns (uint256);

    /// @notice Settled (soulbound) token id for a series (= `keccak256("SETTLED", seriesId)`). Pure helper.
    /// @param seriesId Series identifier.
    /// @return The Settled token id.
    function settledTokenId(uint32 seriesId) external pure returns (uint256);

    /// @notice Both token ids for a series in one call.
    /// @param seriesId Series identifier.
    /// @return issued The Issued token id.
    /// @return settled The Settled token id.
    function tokenIds(uint32 seriesId) external pure returns (uint256 issued, uint256 settled);

    /// @notice Token classification (Issued/Settled) for a token id.
    /// @param tokenId Token id to classify.
    /// @return The token classification.
    function statusOf(uint256 tokenId) external view returns (IntexStatus);

    /// @notice Read series data by series id.
    /// @param seriesId Series identifier.
    /// @return The full series data for the Issued token id.
    function readData(uint32 seriesId) external view returns (SeriesData memory);

    /// @notice Issued and Settled balances for a holder in a given series.
    /// @param seriesId Series identifier.
    /// @param holder Holder address to read.
    /// @return The holder's Issued and Settled balance pair.
    function holderBalances(uint32 seriesId, address holder) external view returns (HolderBalances memory);

    /// @notice Paginated slice of Issued-token holders for a series, with both their Issued
    ///         and Settled balances surfaced in one call.
    /// @dev Iterates `_seriesHolders[issuedTokenId]` only; addresses that hold a Settled
    ///      balance but no Issued balance are not enumerated by this view (no in-tree caller
    ///      currently relies on that — a dedicated Settled-holders view can be added later
    ///      if needed). `limit` must be > 0.
    /// @param seriesId Series identifier.
    /// @param offset Index into `_seriesHolders[issuedTokenId]`.
    /// @param limit Maximum slice length to return.
    /// @return holders Holder addresses in the requested slice.
    /// @return issuedBalances Issued balances parallel to `holders`.
    /// @return settledBalances Settled balances parallel to `holders` (read at the same block).
    /// @return total Length of `_seriesHolders[issuedTokenId]` at call time.
    function getIssuedHoldersWithBalances(uint32 seriesId, uint256 offset, uint256 limit)
        external
        view
        returns (
            address[] memory holders,
            uint256[] memory issuedBalances,
            uint256[] memory settledBalances,
            uint256 total
        );

    /// @notice Total supply for a specific token id.
    /// @param tokenId Token id to read.
    /// @return The total supply of that token id across all holders.
    function totalSupply(uint256 tokenId) external view returns (uint256);

    /// @notice Token URI with on-chain metadata.
    /// @param tokenId Token id to render.
    /// @return The token URI containing on-chain metadata.
    function uri(uint256 tokenId) external view returns (string memory);

    /// @notice Amount won at auction for a specific address in a series (recorded at mint, never changes).
    /// @param seriesId Series identifier.
    /// @param account Address to read.
    /// @return The amount won at auction for `account` in the series.
    function getAuctionWonCount(uint32 seriesId, address account) external view returns (uint16);

    // --- Enumerable reads ---

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

    /// @notice All series (token ids) owned by an address.
    /// @param owner Owner address to read.
    /// @return The token ids the owner holds a balance in.
    function getOwnedSeries(address owner) external view returns (uint256[] memory);

    /// @notice Owned series with pagination.
    /// @param owner Owner address to read.
    /// @param offset Index into the owner's owned-series array.
    /// @param limit Maximum slice length to return.
    /// @return series The requested slice of owned token ids.
    /// @return total Total number of distinct series owned by `owner`.
    function getOwnedSeriesPaginated(address owner, uint256 offset, uint256 limit)
        external
        view
        returns (uint256[] memory series, uint256 total);

    /// @notice Number of distinct series owned by an address.
    /// @param owner Owner address to read.
    /// @return The count of distinct series owned by `owner`.
    function ownedSeriesCount(address owner) external view returns (uint256);

    /// @notice Total Intex balance for an address across all series.
    /// @param owner Owner address to read.
    /// @return The owner's total Intex balance across all series.
    function totalBalance(address owner) external view returns (uint256);

    /// @notice Owned series with their balances for an address.
    /// @param owner Owner address to read.
    /// @return ownedTokenIds The token ids the owner holds.
    /// @return balances Balances parallel to `ownedTokenIds`.
    function getOwnedSeriesWithBalances(address owner)
        external
        view
        returns (uint256[] memory ownedTokenIds, uint256[] memory balances);

    /// @notice All holder addresses for a given series token id.
    /// @param tokenId Series token id to read.
    /// @return holders The holder addresses for that token id.
    function getSeriesHolders(uint256 tokenId) external view returns (address[] memory holders);

    /// @notice All holders and their balances for a given series token id.
    /// @param tokenId Series token id to read.
    /// @return holders The holder addresses for that token id.
    /// @return balances Balances parallel to `holders`.
    function getSeriesHoldersWithBalances(uint256 tokenId)
        external
        view
        returns (address[] memory holders, uint256[] memory balances);

    /// @notice Number of unique holders for a given series token id.
    /// @param tokenId Series token id to read.
    /// @return The count of unique holders for that token id.
    function seriesHolderCount(uint256 tokenId) external view returns (uint256);
}
