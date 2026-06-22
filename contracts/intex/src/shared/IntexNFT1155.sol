// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1155Upgradeable} from "@openzeppelin/contracts-upgradeable/token/ERC1155/ERC1155Upgradeable.sol";
import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {IIntexNFT1155} from "./interfaces/IIntexNFT1155.sol";
import {IERC1155Bridgeable} from "./interfaces/IERC1155Bridgeable.sol";

/**
 * @title IntexNFT1155
 * @author Outbe
 * @notice ERC1155 representation of Intex - a conditional right to obtain promis.
 *
 * @dev UUPS upgradeable: deployed behind an ERC1967 proxy, configured via `initialize`.
 * @dev One auction produces one series with shared parameters for all winners.
 * @dev State transitions affect the entire series simultaneously (O(1) gas).
 * @dev Series lifecycle: Issued -> Qualified -> Called.
 *      Expiration after the call deadline is signalled by the SeriesExpired event;
 *      it is not a distinct on-chain state.
 * @dev Each series has two token ids: issued = `uint256(seriesId)`,
 *      settled = `keccak256("SETTLED", seriesId)`.
 */
contract IntexNFT1155 is ERC1155Upgradeable, AccessControlUpgradeable, UUPSUpgradeable, IIntexNFT1155 {
    /// @notice Bridge relayer role; gates series lifecycle, mint/mintBatch, expireSeries, and
    ///         bridge crosschainBurn/crosschainMint.
    bytes32 public constant RELAYER_ROLE = keccak256("RELAYER_ROLE");
    /// @notice Settlement contract role; allowed to call `settle` (burn Issued + mint Settled).
    bytes32 public constant SETTLEMENT_ROLE = keccak256("SETTLEMENT_ROLE");
    /// @notice Promis facade role; allowed to call `burnSettled`.
    bytes32 public constant PROMIS_ROLE = keccak256("PROMIS_ROLE");
    /// @notice System relayer role; allowed to drive the system bridge during the `Called` window.
    /// @dev Holders of this role can `crosschainBurn` even while the series is `Called`. Regular `RELAYER_ROLE`
    ///      can only `crosschainBurn` while the series is `Qualified`.
    bytes32 public constant SYSTEM_RELAYER_ROLE = keccak256("SYSTEM_RELAYER_ROLE");

    /// @notice Upper bound on a series call period. Exposed so integrators can read the bound and
    ///         tests can assert against it rather than an inline literal.
    uint32 public constant override MAX_INTEX_CALL_PERIOD = uint32(365 days);

    /// @notice Maximum byte length of `collectionDescription`. Bounds the cost of building every
    ///         token's metadata document so an over-long description cannot inflate `tokenURI`
    ///         view gas into a DoS. Internal (no public getter) to conserve EIP-170 runtime size.
    uint256 internal constant MAX_COLLECTION_DESCRIPTION_BYTES = 512;

    /// @dev Domain prefix for `settledTokenId` derivation; isolates Settled ids from the
    ///      issued token-id space.
    bytes constant _SETTLED_DOMAIN = bytes("SETTLED");

    /// @custom:storage-location erc7201:outbe.intex.IntexNFT1155
    struct IntexNFT1155Storage {
        /// @dev Collection-level description string.
        string collectionDescription;
        /// @dev Series-level data, stored per token id. One entry for the Issued token id
        ///      (full series fields) and one for the Settled token id (`status` + `totalSupply`).
        mapping(uint256 tokenId => IIntexNFT1155.SeriesData) seriesData;
        /// @dev Amount won at auction per address per token id (recorded at mint, never changes).
        mapping(uint256 tokenId => mapping(address account => uint16 count)) auctionWonCount;
        /// @dev Array of all token IDs (series) that have been created.
        uint256[] allSeries;
        /// @dev Per-owner array of owned token IDs (series with balance > 0).
        mapping(address owner => uint256[]) ownedSeries;
        /// @dev Index of token ID in ownedSeries[owner] array (for efficient removal).
        mapping(address owner => mapping(uint256 tokenId => uint256 index)) ownedSeriesIndex;
        /// @dev Whether owner has a specific token ID in their ownedSeries.
        mapping(address owner => mapping(uint256 tokenId => bool owns)) ownsToken;
        /// @dev Total balance across all series for each owner.
        mapping(address owner => uint256 balance) totalBalance;
        /// @dev Per-series array of holder addresses (addresses with balance > 0).
        mapping(uint256 tokenId => address[]) seriesHolders;
        /// @dev Index of holder in seriesHolders[tokenId] array (for efficient swap-and-pop removal).
        mapping(uint256 tokenId => mapping(address holder => uint256 index)) seriesHolderIndex;
        /// @dev Whether address is in seriesHolders[tokenId].
        mapping(uint256 tokenId => mapping(address holder => bool isHolder)) isSeriesHolder;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexNFT1155")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xe941cbaf65abb9f7003c3006add9c5d12ba7e339abdf88d4afd5defeb8932900;

    function _s() private pure returns (IntexNFT1155Storage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    /// @notice Initializes the proxy with its role holders.
    /// @param defaultAdmin Receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address defaultAdmin) external initializer {
        if (defaultAdmin == address(0)) revert ZeroAddress("defaultAdmin", defaultAdmin);

        __ERC1155_init("");
        __AccessControl_init();
        __UUPSUpgradeable_init();

        _grantRole(DEFAULT_ADMIN_ROLE, defaultAdmin);
    }

    /// @dev Upgrades are gated by the admin role.
    /// @param newImplementation Address of the implementation the proxy switches to.
    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    /// @notice Collection-level description string.
    /// @return The description set via `setCollectionMetadata`.
    function collectionDescription() external view returns (string memory) {
        return _s().collectionDescription;
    }

    /// @notice Series-level data, stored per token id. Flattened to match the original
    ///         public-mapping getter ABI.
    function seriesData(uint256 tokenId)
        external
        view
        returns (
            uint32 issuedAt,
            uint32 calledAt,
            uint32 intexCallPeriod,
            uint32 totalSupply,
            uint32 issuedIntexCount,
            uint32 mintedCount,
            IIntexNFT1155.IntexStatus status,
            IIntexNFT1155.IntexState state
        )
    {
        IIntexNFT1155.SeriesData storage d = _s().seriesData[tokenId];
        return (
            d.issuedAt,
            d.calledAt,
            d.intexCallPeriod,
            d.totalSupply,
            d.issuedIntexCount,
            d.mintedCount,
            d.status,
            d.state
        );
    }

    /// @notice Amount won at auction per address per token id (recorded at mint, never changes).
    /// @param tokenId Issued token id.
    /// @param account Auction winner address.
    /// @return The recorded won amount.
    function auctionWonCount(uint256 tokenId, address account) external view returns (uint16) {
        return _s().auctionWonCount[tokenId][account];
    }

    /// @inheritdoc IIntexNFT1155
    function createSeries(uint32 seriesId, uint32 issuedIntexCount, uint32 intexCallPeriod)
        external
        onlyRole(RELAYER_ROLE)
    {
        IntexNFT1155Storage storage $ = _s();
        uint256 iTok = uint256(seriesId);

        if ($.seriesData[iTok].issuedAt != 0) {
            revert TokenAlreadyExists(iTok);
        }

        // The cap is part of the series's birth identity; a zero cap would mean "a series no
        // one can mint into," which never matches an auction-cleared result.
        if (issuedIntexCount == 0) revert ZeroIssuedIntexCount();

        // Default to 21 days when zero is provided; cap at MAX_INTEX_CALL_PERIOD to guard against accidents.
        uint32 effectiveCallPeriod = intexCallPeriod == 0 ? uint32(21 days) : intexCallPeriod;
        if (effectiveCallPeriod > MAX_INTEX_CALL_PERIOD) revert InvalidCallPeriod(intexCallPeriod);

        $.seriesData[iTok] = IIntexNFT1155.SeriesData({
            issuedAt: uint32(block.timestamp),
            calledAt: 0,
            intexCallPeriod: effectiveCallPeriod,
            totalSupply: 0,
            issuedIntexCount: issuedIntexCount,
            mintedCount: 0,
            status: IIntexNFT1155.IntexStatus.Issued,
            state: IIntexNFT1155.IntexState.Issued
        });

        // Register the Settled token id so reverse lookups and status checks work for either class.
        uint256 sTok = _settledTokenId(seriesId);
        $.seriesData[sTok].status = IIntexNFT1155.IntexStatus.Settled;

        // Series remain in allSeries permanently even after supply reaches 0 —
        // preserves the historical record and avoids O(n) removal. Only the Issued id is
        // enumerated; clients derive the Settled id via `settledTokenId(seriesId)`.
        $.allSeries.push(iTok);

        emit MetadataUpdate(iTok);
    }

    /// @inheritdoc IIntexNFT1155
    function mint(address to, uint256 quantity, uint32 seriesId) external onlyRole(RELAYER_ROLE) {
        if (to == address(0)) {
            revert ZeroAddress("to", to);
        }

        IntexNFT1155Storage storage $ = _s();
        uint256 tokenId = uint256(seriesId);
        IIntexNFT1155.SeriesData storage data = $.seriesData[tokenId];
        if (data.issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }

        // A per-recipient mint quantity is one bidder's auction win, bounded by their bid's
        // `intexQuantity` (uint16); keeps the ERC1155 balance, `totalSupply` and `auctionWonCount` consistent.
        if (quantity > type(uint16).max) revert QuantityTooLarge(quantity);

        // Cap is enforced against the monotonic `mintedCount`, not live `totalSupply`. A burn
        // (crosschainBurn/settle/expireSeries) reduces totalSupply but leaves mintedCount untouched, so
        // a burn-then-remint cycle cannot reopen cap room (R-01). The intermediate is widened
        // to uint256 so a series with `issuedIntexCount` near `type(uint32).max` surfaces the
        // typed `SupplyCapExceeded` revert rather than a raw arithmetic panic on overflow.
        uint256 newMinted = uint256(data.mintedCount) + quantity;
        if (newMinted > data.issuedIntexCount) {
            revert SupplyCapExceeded(seriesId, newMinted, data.issuedIntexCount);
        }

        // CEI ok: write totalSupply and mintedCount before _mint so the ERC1155 receiver
        // callback observes a consistent (totalSupply == Σ balanceOf, mintedCount == cumulative)
        // snapshot — closes the read-only-reentrancy window. Casts are safe because the cap
        // check above bounded `newMinted ≤ issuedIntexCount ≤ type(uint32).max`.
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by cap check above
        data.totalSupply += uint32(quantity);
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by cap check above
        data.mintedCount = uint32(newMinted);
        _mint(to, tokenId, quantity, "");

        if ($.auctionWonCount[tokenId][to] == 0) {
            // forge-lint: disable-next-line(unsafe-typecast) -- quantity bounded to uint16 above
            $.auctionWonCount[tokenId][to] = uint16(quantity);
        }

        emit IntexIssued(msg.sender, tokenId, to, quantity);
    }

    /// @inheritdoc IIntexNFT1155
    function mintBatch(address[] calldata recipients, uint256[] calldata quantities, uint32 seriesId)
        external
        onlyRole(RELAYER_ROLE)
    {
        if (recipients.length != quantities.length) {
            revert ArrayLengthMismatch(recipients.length, quantities.length);
        }
        if (recipients.length == 0) {
            revert EmptyArray();
        }

        IntexNFT1155Storage storage $ = _s();
        uint256 tokenId = uint256(seriesId);
        IIntexNFT1155.SeriesData storage data = $.seriesData[tokenId];

        if (data.issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }

        // Pre-validate every recipient and per-quantity bound before any state mutation, and
        // sum the batch so the supply cap is enforced once with the full batch total (not
        // partially mid-loop). This makes the batch all-or-nothing wrt. the cap.
        uint256 batchSum = 0;
        for (uint256 i = 0; i < recipients.length; i++) {
            if (recipients[i] == address(0)) {
                revert ZeroAddress("recipient", recipients[i]);
            }
            if (quantities[i] > type(uint16).max) revert QuantityTooLarge(quantities[i]);
            batchSum += quantities[i];
        }

        // Cap is enforced against `mintedCount` (cumulative, monotonic). The intermediate is
        // widened to uint256 so a series with `issuedIntexCount` near `type(uint32).max`
        // surfaces `SupplyCapExceeded` rather than an arithmetic panic. The cap field is
        // itself uint32, so any `newMinted > issuedIntexCount` covers `batchSum > uint32.max`
        // implicitly — no separate batchSum overflow guard is needed.
        uint256 newMinted = uint256(data.mintedCount) + batchSum;
        if (newMinted > data.issuedIntexCount) {
            revert SupplyCapExceeded(seriesId, newMinted, data.issuedIntexCount);
        }
        // CEI ok: write the post-batch totals before the per-recipient _mint loop so each
        // ERC1155 receiver callback sees (totalSupply == Σ balanceOf, mintedCount == cumulative)
        // for the final batch. Casts are safe because `newMinted ≤ issuedIntexCount ≤ uint32.max`.
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by cap check above
        data.totalSupply += uint32(batchSum);
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by cap check above
        data.mintedCount = uint32(newMinted);

        for (uint256 i = 0; i < recipients.length; i++) {
            if (quantities[i] == 0) {
                continue;
            }
            _mint(recipients[i], tokenId, quantities[i], "");

            if ($.auctionWonCount[tokenId][recipients[i]] == 0) {
                // forge-lint: disable-next-line(unsafe-typecast) -- quantity bounded to uint16 above
                $.auctionWonCount[tokenId][recipients[i]] = uint16(quantities[i]);
            }

            emit IntexIssued(msg.sender, tokenId, recipients[i], quantities[i]);
        }
    }

    /// @inheritdoc IIntexNFT1155
    function markQualified(uint32 seriesId) external onlyRole(RELAYER_ROLE) {
        uint256 tokenId = uint256(seriesId);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }
        if (data.state != IIntexNFT1155.IntexState.Issued) {
            revert InvalidState(uint8(IIntexNFT1155.IntexState.Issued), uint8(data.state));
        }

        IIntexNFT1155.IntexState previousState = data.state;
        data.state = IIntexNFT1155.IntexState.Qualified;

        emit IntexStatusUpdated(
            msg.sender, tokenId, previousState, IIntexNFT1155.IntexState.Qualified, uint32(block.timestamp), 0
        );
        emit MetadataUpdate(tokenId);
    }

    /// @inheritdoc IIntexNFT1155
    function markCalled(uint32 seriesId) external onlyRole(RELAYER_ROLE) {
        uint256 tokenId = uint256(seriesId);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }
        // Allow Issued -> Called and Qualified -> Called; the relayer drives the qualification oracle.
        if (data.state != IIntexNFT1155.IntexState.Issued && data.state != IIntexNFT1155.IntexState.Qualified) {
            revert InvalidState(uint8(IIntexNFT1155.IntexState.Qualified), uint8(data.state));
        }

        uint32 calledAt = uint32(block.timestamp);
        IIntexNFT1155.IntexState previousState = data.state;
        data.state = IIntexNFT1155.IntexState.Called;
        data.calledAt = calledAt;

        uint32 derivedDeadline = calledAt + data.intexCallPeriod;

        emit IntexStatusUpdated(
            msg.sender, tokenId, previousState, IIntexNFT1155.IntexState.Called, calledAt, derivedDeadline
        );
        emit MetadataUpdate(tokenId);
    }

    /// @inheritdoc IIntexNFT1155
    function expireSeries(uint32 seriesId, uint256 limit) external onlyRole(RELAYER_ROLE) {
        if (limit == 0) revert ZeroLimit();

        IntexNFT1155Storage storage $ = _s();
        uint256 tokenId = uint256(seriesId);
        IIntexNFT1155.SeriesData storage data = $.seriesData[tokenId];

        if (data.issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }
        if (data.state != IIntexNFT1155.IntexState.Called) {
            revert InvalidState(uint8(IIntexNFT1155.IntexState.Called), uint8(data.state));
        }
        uint32 derivedDeadline = data.calledAt + data.intexCallPeriod;
        if (data.calledAt == 0 || block.timestamp <= derivedDeadline) {
            revert SeriesNotYetExpired(derivedDeadline, uint32(block.timestamp));
        }
        // Idempotency guard: once every holder has been swept, `totalSupply` is zero and
        // a second expiration call is meaningless. Reverting here keeps the SeriesExpired
        // event single-shot for indexers without introducing a dedicated terminal flag.
        if (data.totalSupply == 0) {
            revert NothingToExpire();
        }

        // Pagination: always sweep from index 0 of the live holder array. The existing
        // `_update → _removeSeriesHolder` swap-and-pop flow shrinks the array as each
        // burn drops a balance to zero, so the next call naturally picks up where this
        // one left off without an explicit cursor.
        uint256 remaining = $.seriesHolders[tokenId].length;
        uint256 toProcess = limit < remaining ? limit : remaining;
        uint256 burned = 0;
        for (uint256 i = 0; i < toProcess; i++) {
            // Each `_burn` triggers `_removeSeriesHolder`, swapping the tail into slot 0.
            // Reading `seriesHolders[tokenId][0]` on every iteration is therefore the
            // correct way to advance through the shrinking page.
            address holder = $.seriesHolders[tokenId][0];
            uint256 bal = balanceOf(holder, tokenId);
            if (bal > 0) {
                _burn(holder, tokenId, bal);
                // forge-lint: disable-next-line(unsafe-typecast) -- bal <= totalSupply (uint32) by construction
                burned += bal;
            }
        }

        if (burned > 0) {
            // forge-lint: disable-next-line(unsafe-typecast) -- burned == Σ bal, each ≤ totalSupply (uint32)
            data.totalSupply -= uint32(burned);
        }

        // Final page <=> the holder array drained to empty on this call. Mid-page progress
        // is surfaced via SeriesExpiredProgress so off-chain indexers can track sweeps
        // without polling the holders array. State stays Called.
        if ($.seriesHolders[tokenId].length == 0) {
            emit SeriesExpired(tokenId, msg.sender);
        } else {
            emit SeriesExpiredProgress(seriesId, toProcess);
        }
        emit MetadataUpdate(tokenId);
    }

    /// @inheritdoc IERC1155Bridgeable
    /// @dev Bridge crosschainBurn gating:
    ///      - Settled token ids are soulbound — always reverts.
    ///      - Series state `Issued` (initial): bridge is disallowed.
    ///      - Series state `Qualified`: bridge allowed for `RELAYER_ROLE`.
    ///      - Series state `Called`: bridge allowed only for `SYSTEM_RELAYER_ROLE`
    ///        (the system bridge that migrates balances during the call window).
    function crosschainBurn(address from, uint256 tokenId, uint256 amount) external onlyRole(RELAYER_ROLE) {
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.status == IIntexNFT1155.IntexStatus.Settled) {
            revert BridgeOnSettledForbidden(tokenId);
        }
        if (data.issuedAt == 0) revert NonexistentToken(tokenId);
        if (from == address(0)) revert ZeroAddress("from", from);

        IIntexNFT1155.IntexState state = data.state;
        if (state == IIntexNFT1155.IntexState.Issued) {
            revert BridgeStateForbidden(tokenId, uint8(state));
        }
        if (state == IIntexNFT1155.IntexState.Called) {
            if (!hasRole(SYSTEM_RELAYER_ROLE, msg.sender)) {
                revert BridgeStateForbidden(tokenId, uint8(state));
            }
            // Bridge moves are confined to the call window: once `calledAt + intexCallPeriod`
            // passes the series is settlement-complete and balances must stay frozen, otherwise
            // a system relayer could keep moving (and `crosschainMint` could re-inflate) post-lifecycle.
            uint32 derivedDeadline = data.calledAt + data.intexCallPeriod;
            if (block.timestamp > derivedDeadline) {
                revert BridgeAfterDeadline(tokenId, derivedDeadline);
            }
        }

        // CEI ok: write before _burn for symmetry with mint. _burn fires no acceptance callback
        // (OZ ERC1155 skips it when to == address(0)), so no read-only-reentrancy surface here.
        // forge-lint: disable-next-line(unsafe-typecast) -- amount <= balance <= totalSupply (uint32); _burn reverts otherwise
        data.totalSupply -= uint32(amount);
        _burn(from, tokenId, amount);
        emit MetadataUpdate(tokenId);
    }

    /// @inheritdoc IERC1155Bridgeable
    /// @dev Bridge crosschainMint mirrors `crosschainBurn`. Same state gates apply: `Issued` is always rejected,
    ///      `Called` is reserved for `SYSTEM_RELAYER_ROLE`.
    function crosschainMint(address to, uint256 tokenId, uint256 amount) external onlyRole(RELAYER_ROLE) {
        if (to == address(0)) revert ZeroAddress("to", to);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.status == IIntexNFT1155.IntexStatus.Settled) {
            revert BridgeOnSettledForbidden(tokenId);
        }
        if (data.issuedAt == 0) revert NonexistentToken(tokenId);

        IIntexNFT1155.IntexState state = data.state;
        if (state == IIntexNFT1155.IntexState.Issued) {
            revert BridgeStateForbidden(tokenId, uint8(state));
        }
        if (state == IIntexNFT1155.IntexState.Called) {
            if (!hasRole(SYSTEM_RELAYER_ROLE, msg.sender)) {
                revert BridgeStateForbidden(tokenId, uint8(state));
            }
            // Mirror of `crosschainBurn`: no bridge-in past the settlement deadline. Without this a
            // `crosschainMint` after `expireSeries` drained the series could re-inflate `totalSupply`
            // (capped by `issuedIntexCount`, but still a post-lifecycle mutation).
            uint32 derivedDeadline = data.calledAt + data.intexCallPeriod;
            if (block.timestamp > derivedDeadline) {
                revert BridgeAfterDeadline(tokenId, derivedDeadline);
            }
        }

        // A crosschainMinted balance can be a holder's full transferable balance (<= totalSupply, uint32).
        if (amount > type(uint32).max) revert QuantityTooLarge(amount);

        // Bridge-in cap: enforce `totalSupply + amount ≤ issuedIntexCount` at all times. CrosschainMint
        // intentionally does NOT bump `mintedCount` — cross-chain returns of already-issued
        // tokens are legitimate and must not consume primary-mint capacity. The live-supply
        // invariant suffices because cumulative primary issuance is bounded at mint/mintBatch.
        // Intermediate widened to uint256 so the cap revert surfaces as `SupplyCapExceeded`
        // even at the `issuedIntexCount == type(uint32).max` boundary.
        uint256 newTotal = uint256(data.totalSupply) + amount;
        // The Issued entry carries the cap; for the Settled token id it is zero and the
        // status guard above already rejected. Only the Issued path reaches here.
        if (newTotal > data.issuedIntexCount) {
            // forge-lint: disable-next-line(unsafe-typecast) -- Issued tokenId == uint256(seriesId)
            revert SupplyCapExceeded(uint32(tokenId), newTotal, data.issuedIntexCount);
        }

        // CEI ok: write totalSupply before _mint (see mint()). Cast is safe because the cap
        // check bounded `newTotal ≤ issuedIntexCount ≤ uint32.max`.
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by cap check above
        data.totalSupply = uint32(newTotal);
        _mint(to, tokenId, amount, "");
        emit MetadataUpdate(tokenId);
    }

    /// @inheritdoc IIntexNFT1155
    function settle(uint32 seriesId, address from, address to, uint256 amount) external onlyRole(SETTLEMENT_ROLE) {
        if (from == address(0)) revert ZeroAddress("from", from);
        if (to == address(0)) revert ZeroAddress("to", to);
        if (amount == 0) revert ZeroAmount();

        IntexNFT1155Storage storage $ = _s();
        uint256 iTok = uint256(seriesId);
        IIntexNFT1155.SeriesData storage data = $.seriesData[iTok];
        if (data.issuedAt == 0) revert NonexistentToken(iTok);

        if (data.state != IIntexNFT1155.IntexState.Qualified && data.state != IIntexNFT1155.IntexState.Called) {
            revert InvalidStateForSettle(uint8(data.state));
        }

        if (data.state == IIntexNFT1155.IntexState.Called) {
            // No new Settled tokens past the call window (mirrors the crosschainBurn/crosschainMint freeze).
            uint32 derivedDeadline = data.calledAt + data.intexCallPeriod;
            if (block.timestamp > derivedDeadline) {
                revert SettleAfterDeadline(iTok, derivedDeadline);
            }
        }

        uint256 sTok = _settledTokenId(seriesId);

        // CEI ok: update both Issued and Settled totalSupply mirrors before the external _mint
        // callback fires — keeps (totalSupply == Σ balanceOf) consistent mid-callback.
        // Burn `amount` Issued from `from` and mint the same `amount` of Settled to `to`.
        // forge-lint: disable-next-line(unsafe-typecast) -- amount <= issued balance <= totalSupply (uint32); _burn reverts otherwise
        data.totalSupply -= uint32(amount);
        _burn(from, iTok, amount);

        // forge-lint: disable-next-line(unsafe-typecast) -- amount mirrors the issued amount burned above
        $.seriesData[sTok].totalSupply += uint32(amount);
        _mint(to, sTok, amount, "");

        emit IntexSettled(seriesId, to, amount);
        emit MetadataUpdate(iTok);
        emit MetadataUpdate(sTok);
    }

    /// @inheritdoc IIntexNFT1155
    function burnSettled(address holder, uint32 seriesId, uint256 amount) external onlyRole(PROMIS_ROLE) {
        if (holder == address(0)) revert ZeroAddress("holder", holder);
        if (amount == 0) revert ZeroAmount();

        IntexNFT1155Storage storage $ = _s();
        uint256 iTok = uint256(seriesId);
        IIntexNFT1155.SeriesData storage iData = $.seriesData[iTok];
        // Series must exist; we look up via the Issued id storage.
        if (iData.issuedAt == 0) revert NonexistentToken(iTok);

        // Mirror `settle`'s precondition: Settled balances only exist after a settle, which
        // is only permitted from Qualified or Called. Making the gate explicit (instead of
        // relying on `_burn`'s zero-balance revert) keeps a future change that pre-mints
        // Settled tokens — e.g. an airdrop variant — from accidentally opening an early-burn
        // window. The gate is `state ∈ {Qualified, Called}`, not a fictional Settled state value.
        if (iData.state != IIntexNFT1155.IntexState.Qualified && iData.state != IIntexNFT1155.IntexState.Called) {
            revert InvalidState(uint8(IIntexNFT1155.IntexState.Qualified), uint8(iData.state));
        }

        uint256 sTok = _settledTokenId(seriesId);
        // CEI ok: write before _burn for symmetry with mint; _burn fires no acceptance callback
        // (to == address(0)), so no read-only-reentrancy surface here.
        // forge-lint: disable-next-line(unsafe-typecast) -- amount <= settled balance <= totalSupply (uint32); _burn reverts otherwise
        $.seriesData[sTok].totalSupply -= uint32(amount);
        _burn(holder, sTok, amount);

        emit IntexCompleted(seriesId, holder, amount);
        emit MetadataUpdate(sTok);
    }

    /// @inheritdoc IIntexNFT1155
    function setCollectionMetadata(string calldata description) external onlyRole(DEFAULT_ADMIN_ROLE) {
        if (bytes(description).length > MAX_COLLECTION_DESCRIPTION_BYTES) {
            revert CollectionDescriptionTooLong();
        }
        _s().collectionDescription = description;
        emit CollectionMetadataUpdated(description);
    }

    /// @inheritdoc IIntexNFT1155
    function issuedTokenId(uint32 seriesId) external pure returns (uint256) {
        return uint256(seriesId);
    }

    /// @inheritdoc IIntexNFT1155
    function settledTokenId(uint32 seriesId) external pure returns (uint256) {
        return _settledTokenId(seriesId);
    }

    /// @inheritdoc IIntexNFT1155
    function tokenIds(uint32 seriesId) external pure returns (uint256 issued, uint256 settled) {
        return (uint256(seriesId), _settledTokenId(seriesId));
    }

    /// @dev Pure helper used internally and exposed via `settledTokenId`.
    function _settledTokenId(uint32 seriesId) internal pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked(_SETTLED_DOMAIN, seriesId)));
    }

    /// @inheritdoc IIntexNFT1155
    function statusOf(uint256 tokenId) external view returns (IIntexNFT1155.IntexStatus) {
        return _s().seriesData[tokenId].status;
    }

    /// @inheritdoc IIntexNFT1155
    function readData(uint32 seriesId) external view returns (IIntexNFT1155.SeriesData memory) {
        IntexNFT1155Storage storage $ = _s();
        uint256 tokenId = uint256(seriesId);
        // `issuedAt == 0` is the canonical existence sentinel for seriesData entries.
        // slither-disable-next-line incorrect-equality
        if ($.seriesData[tokenId].issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }
        return $.seriesData[tokenId];
    }

    /// @inheritdoc IIntexNFT1155
    function holderBalances(uint32 seriesId, address holder)
        external
        view
        returns (IIntexNFT1155.HolderBalances memory)
    {
        uint256 iTok = uint256(seriesId);
        uint256 sTok = _settledTokenId(seriesId);
        return IIntexNFT1155.HolderBalances({
            issued: uint16(balanceOf(holder, iTok)), settled: uint16(balanceOf(holder, sTok))
        });
    }

    /// @inheritdoc IIntexNFT1155
    function getIssuedHoldersWithBalances(uint32 seriesId, uint256 offset, uint256 limit)
        external
        view
        returns (
            address[] memory holders,
            uint256[] memory issuedBalances,
            uint256[] memory settledBalances,
            uint256 total
        )
    {
        if (limit == 0) revert ZeroLimit();

        uint256 iTok = uint256(seriesId);
        uint256 sTok = _settledTokenId(seriesId);

        address[] storage allHolders = _s().seriesHolders[iTok];
        total = allHolders.length;
        if (offset >= total) {
            return (new address[](0), new uint256[](0), new uint256[](0), total);
        }

        // Clip the slice length to the remaining tail before adding to `offset` — prevents
        // a checked-arithmetic panic when callers pass `type(uint256).max` as a sentinel
        // (overflow). `offset < total` is guaranteed by the early-return above.
        uint256 sliceLen = total - offset;
        if (limit < sliceLen) sliceLen = limit;

        holders = new address[](sliceLen);
        issuedBalances = new uint256[](sliceLen);
        settledBalances = new uint256[](sliceLen);

        for (uint256 i = 0; i < sliceLen; i++) {
            address h = allHolders[offset + i];
            holders[i] = h;
            issuedBalances[i] = balanceOf(h, iTok);
            settledBalances[i] = balanceOf(h, sTok);
        }
        return (holders, issuedBalances, settledBalances, total);
    }

    /// @inheritdoc IIntexNFT1155
    function totalSupply(uint256 tokenId) external view returns (uint256) {
        return _s().seriesData[tokenId].totalSupply;
    }

    /// @inheritdoc IIntexNFT1155
    function getAuctionWonCount(uint32 seriesId, address account) external view returns (uint16) {
        return _s().auctionWonCount[uint256(seriesId)][account];
    }

    /// @inheritdoc IIntexNFT1155
    function uri(uint256 tokenId) public view override(ERC1155Upgradeable, IIntexNFT1155) returns (string memory) {
        return super.uri(tokenId);
    }

    /// @notice ERC1155 transfer hook: enforces soulbound Settled tokens and maintains the
    ///         owned-series / series-holder enumeration indexes.
    /// @dev Transfer lock and soulbound enforcement.
    ///      - Mint/burn paths (from/to address(0)) are always allowed (settle, burnSettled,
    ///        bridge crosschainBurn/crosschainMint on Issued, expireSeries, mint/mintBatch).
    ///      - Holder-to-holder transfers:
    ///          * Settled token ids are soulbound — always reverts.
    ///          * Issued token ids are transferable in every series state
    ///            (Issued, Qualified, Called). Bridge gating is separate and lives in
    ///            `crosschainBurn` / `crosschainMint`.
    /// @param from Sender address (address(0) for mints).
    /// @param to Receiver address (address(0) for burns).
    /// @param ids Array of token IDs.
    /// @param values Array of amounts.
    function _update(address from, address to, uint256[] memory ids, uint256[] memory values) internal override {
        IntexNFT1155Storage storage $ = _s();
        if (from != address(0) && to != address(0)) {
            for (uint256 i = 0; i < ids.length; i++) {
                if ($.seriesData[ids[i]].status == IIntexNFT1155.IntexStatus.Settled) {
                    revert SoulboundSettled(ids[i]);
                }
            }
        }

        // Snapshot pre-transfer balances — checked BEFORE super._update, verified AFTER
        // to handle duplicate tokenIds in batch correctly.
        bool[] memory fromHadTokens = new bool[](ids.length);
        bool[] memory toHadTokens = new bool[](ids.length);

        for (uint256 i = 0; i < ids.length; i++) {
            if (values[i] == 0) continue;
            if (from != address(0)) {
                fromHadTokens[i] = balanceOf(from, ids[i]) > 0;
                $.totalBalance[from] -= values[i];
            }
            if (to != address(0)) {
                toHadTokens[i] = balanceOf(to, ids[i]) > 0;
                $.totalBalance[to] += values[i];
            }
        }

        super._update(from, to, ids, values);

        // Post-transfer: add/remove are idempotent, safe for duplicate tokenIds in batch.
        for (uint256 i = 0; i < ids.length; i++) {
            if (values[i] == 0) continue;

            if (from != address(0) && fromHadTokens[i] && balanceOf(from, ids[i]) == 0) {
                _removeOwnedSeries(from, ids[i]);
                _removeSeriesHolder(ids[i], from);
            }
            if (to != address(0) && !toHadTokens[i] && balanceOf(to, ids[i]) > 0) {
                _addOwnedSeries(to, ids[i]);
                _addSeriesHolder(ids[i], to);
            }
        }
    }

    /// @dev Add `tokenId` to `owner`'s owned-series enumeration (idempotent).
    /// @param owner Owner address.
    /// @param tokenId Token ID to add.
    function _addOwnedSeries(address owner, uint256 tokenId) internal {
        IntexNFT1155Storage storage $ = _s();
        if (!$.ownsToken[owner][tokenId]) {
            $.ownedSeriesIndex[owner][tokenId] = $.ownedSeries[owner].length;
            $.ownedSeries[owner].push(tokenId);
            $.ownsToken[owner][tokenId] = true;
        }
    }

    /// @dev Remove `tokenId` from `owner`'s owned-series enumeration (swap-and-pop, idempotent).
    /// @param owner Owner address.
    /// @param tokenId Token ID to remove.
    function _removeOwnedSeries(address owner, uint256 tokenId) internal {
        IntexNFT1155Storage storage $ = _s();
        if ($.ownsToken[owner][tokenId]) {
            uint256 lastIndex = $.ownedSeries[owner].length - 1;
            uint256 tokenIndex = $.ownedSeriesIndex[owner][tokenId];

            if (tokenIndex != lastIndex) {
                uint256 lastTokenId = $.ownedSeries[owner][lastIndex];
                $.ownedSeries[owner][tokenIndex] = lastTokenId;
                $.ownedSeriesIndex[owner][lastTokenId] = tokenIndex;
            }

            $.ownedSeries[owner].pop();
            delete $.ownedSeriesIndex[owner][tokenId];
            $.ownsToken[owner][tokenId] = false;
        }
    }

    /// @dev Add `holder` to series `tokenId`'s holder enumeration (idempotent).
    /// @param tokenId Token ID (series).
    /// @param holder Holder address to add.
    function _addSeriesHolder(uint256 tokenId, address holder) internal {
        IntexNFT1155Storage storage $ = _s();
        if (!$.isSeriesHolder[tokenId][holder]) {
            $.seriesHolderIndex[tokenId][holder] = $.seriesHolders[tokenId].length;
            $.seriesHolders[tokenId].push(holder);
            $.isSeriesHolder[tokenId][holder] = true;
        }
    }

    /// @dev Remove `holder` from series `tokenId`'s holder enumeration (swap-and-pop, idempotent).
    /// @param tokenId Token ID (series).
    /// @param holder Holder address to remove.
    function _removeSeriesHolder(uint256 tokenId, address holder) internal {
        IntexNFT1155Storage storage $ = _s();
        if ($.isSeriesHolder[tokenId][holder]) {
            uint256 lastIndex = $.seriesHolders[tokenId].length - 1;
            uint256 holderIndex = $.seriesHolderIndex[tokenId][holder];

            if (holderIndex != lastIndex) {
                address lastHolder = $.seriesHolders[tokenId][lastIndex];
                $.seriesHolders[tokenId][holderIndex] = lastHolder;
                $.seriesHolderIndex[tokenId][lastHolder] = holderIndex;
            }

            $.seriesHolders[tokenId].pop();
            delete $.seriesHolderIndex[tokenId][holder];
            $.isSeriesHolder[tokenId][holder] = false;
        }
    }

    // --- Enumerable view functions ---

    /// @inheritdoc IIntexNFT1155
    function getAllSeries() external view returns (uint256[] memory) {
        return _s().allSeries;
    }

    /// @inheritdoc IIntexNFT1155
    function getSeriesPaginated(uint256 offset, uint256 limit)
        external
        view
        returns (uint256[] memory series, uint256 total)
    {
        uint256[] storage allSeries = _s().allSeries;
        total = allSeries.length;
        if (offset >= total) return (new uint256[](0), total);

        uint256 end = offset + limit;
        if (end > total) end = total;

        series = new uint256[](end - offset);
        for (uint256 i = offset; i < end; i++) {
            series[i - offset] = allSeries[i];
        }
    }

    /// @inheritdoc IIntexNFT1155
    function totalSeries() external view returns (uint256) {
        return _s().allSeries.length;
    }

    /// @inheritdoc IIntexNFT1155
    function getOwnedSeries(address owner) external view returns (uint256[] memory) {
        return _s().ownedSeries[owner];
    }

    /// @inheritdoc IIntexNFT1155
    function getOwnedSeriesPaginated(address owner, uint256 offset, uint256 limit)
        external
        view
        returns (uint256[] memory series, uint256 total)
    {
        uint256[] storage owned = _s().ownedSeries[owner];
        total = owned.length;
        if (offset >= total) return (new uint256[](0), total);

        uint256 end = offset + limit;
        if (end > total) end = total;

        series = new uint256[](end - offset);
        for (uint256 i = offset; i < end; i++) {
            series[i - offset] = owned[i];
        }
    }

    /// @inheritdoc IIntexNFT1155
    function ownedSeriesCount(address owner) external view returns (uint256) {
        return _s().ownedSeries[owner].length;
    }

    /// @inheritdoc IIntexNFT1155
    function totalBalance(address owner) external view returns (uint256) {
        return _s().totalBalance[owner];
    }

    /// @inheritdoc IIntexNFT1155
    function getOwnedSeriesWithBalances(address owner)
        external
        view
        returns (uint256[] memory ownedTokenIds, uint256[] memory balances)
    {
        ownedTokenIds = _s().ownedSeries[owner];
        balances = new uint256[](ownedTokenIds.length);

        for (uint256 i = 0; i < ownedTokenIds.length; i++) {
            balances[i] = balanceOf(owner, ownedTokenIds[i]);
        }

        return (ownedTokenIds, balances);
    }

    // --- Series holder enumeration (tokenId → holders[]) ---

    /// @inheritdoc IIntexNFT1155
    function getSeriesHolders(uint256 tokenId) external view returns (address[] memory) {
        return _s().seriesHolders[tokenId];
    }

    /// @inheritdoc IIntexNFT1155
    function getSeriesHoldersWithBalances(uint256 tokenId)
        external
        view
        returns (address[] memory holders, uint256[] memory balances)
    {
        holders = _s().seriesHolders[tokenId];
        balances = new uint256[](holders.length);

        for (uint256 i = 0; i < holders.length; i++) {
            balances[i] = balanceOf(holders[i], tokenId);
        }
    }

    /// @inheritdoc IIntexNFT1155
    function seriesHolderCount(uint256 tokenId) external view returns (uint256) {
        return _s().seriesHolders[tokenId].length;
    }

    /// @notice ERC-165 interface detection.
    /// @dev Reports support for `IIntexNFT1155` and `IERC1155Bridgeable` in addition to the
    ///      interfaces advertised by ERC1155 and AccessControl.
    /// @param interfaceId The ERC-165 interface identifier to query.
    /// @return True if the contract implements `interfaceId`.
    function supportsInterface(bytes4 interfaceId)
        public
        view
        override(IERC165, ERC1155Upgradeable, AccessControlUpgradeable)
        returns (bool)
    {
        return interfaceId == type(IIntexNFT1155).interfaceId || interfaceId == type(IERC1155Bridgeable).interfaceId
            || super.supportsInterface(interfaceId);
    }
}
