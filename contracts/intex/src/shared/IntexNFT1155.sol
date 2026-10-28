// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1155Upgradeable} from "@openzeppelin/contracts-upgradeable/token/ERC1155/ERC1155Upgradeable.sol";
import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {IIntexNFT1155} from "./interfaces/IIntexNFT1155.sol";
import {IERC1155Bridgeable} from "./interfaces/IERC1155Bridgeable.sol";
import {IntexMetadata} from "./libs/IntexMetadata.sol";

/**
 * @title IntexNFT1155
 * @author Outbe
 * @notice ERC1155 representation of Intex - a conditional right to obtain promis.
 *
 * @dev UUPS upgradeable: deployed behind an ERC1967 proxy and configured via `initialize`.
 * @dev One auction produces one series with shared parameters for all winners.
 * @dev State transitions affect the entire series simultaneously (O(1) gas).
 * @dev Series lifecycle: Issued -> Called.
 *      Expiry is not an on-chain state. It is derived from `calledAt + callNoticePeriod`
 *      against the clock (issue/settle/bridge gates, metadata rendering).
 * @dev A series' units are the token id `uint112(seriesId)`. Settled units live in the Intex
 *      precompile on Outbe: a settlement burns them here.
 */
contract IntexNFT1155 is ERC1155Upgradeable, AccessControlUpgradeable, UUPSUpgradeable, IIntexNFT1155 {
    /// @notice Bridge relayer role. It gates series lifecycle and bridge crosschainBurn/crosschainMint.
    bytes32 public constant RELAYER_ROLE = keccak256("RELAYER_ROLE");
    /// @notice Auction role. It can call `issueIntex` for a winner.
    bytes32 public constant AUCTION_ROLE = keccak256("AUCTION_ROLE");
    /// @notice Burner role: the IntexFactory and GemFactory precompiles on Outbe. It can call `burnIntex`.
    bytes32 public constant BURNER_ROLE = keccak256("BURNER_ROLE");

    /// @notice Upper bound on how early a `Called` series stops leaving a chain before its settlement
    ///         deadline. The margin is a tenth of the notice period, capped at this value, and leaves a
    ///         hop in flight time to land before the deadline.
    uint256 public constant BRIDGE_CUTOFF = 1 hours;

    /// @custom:storage-location erc7201:outbe.intex.IntexNFT1155
    struct IntexNFT1155Storage {
        /// @dev Series-level data, stored once per series under its token id.
        mapping(uint256 tokenId => IIntexNFT1155.SeriesData) seriesData;
        /// @dev Every series ever created on this chain, in creation order.
        bytes14[] allSeries;
        /// @dev Series ids issued per worldwide day.
        mapping(uint32 worldwideDay => bytes14[] seriesIds) seriesOfDay;
        /// @dev The series a day's currency pair was created under.
        mapping(
            uint32 worldwideDay
                => mapping(uint16 issuanceCurrency => mapping(uint16 referenceCurrency => bytes14 seriesId))
        ) seriesOfPair;
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

        _grantRole(DEFAULT_ADMIN_ROLE, defaultAdmin);
    }

    /// @dev The admin role gates upgrades.
    /// @param newImplementation Address of the implementation the proxy switches to.
    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    /// @inheritdoc IIntexNFT1155
    function worldwideDayOf(bytes14 seriesId) external view returns (uint32) {
        return _s().seriesData[_issuedTokenId(seriesId)].worldwideDay;
    }

    /// @inheritdoc IIntexNFT1155
    function seriesIdsByWorldwideDay(uint32 worldwideDay) external view returns (bytes14[] memory) {
        return _s().seriesOfDay[worldwideDay];
    }

    /// @inheritdoc IIntexNFT1155
    function seriesIdOf(uint32 worldwideDay, uint16 issuanceCurrency, uint16 referenceCurrency)
        external
        view
        returns (bytes14)
    {
        return _s().seriesOfPair[worldwideDay][issuanceCurrency][referenceCurrency];
    }

    /// @inheritdoc IIntexNFT1155
    function createSeries(IIntexNFT1155.CreateSeriesParams calldata params) external onlyRole(RELAYER_ROLE) {
        IntexNFT1155Storage storage $ = _s();
        uint256 iTok = _issuedTokenId(params.seriesId);

        if ($.seriesData[iTok].issuedAt != 0) {
            revert TokenAlreadyExists(iTok);
        }

        // A series is born from an auction that cleared at least one unit.
        if (params.issuedUnits == 0) revert ZeroIssuedUnits();

        // Zero is how this contract reads "no such series". A future stamp would
        // postpone the call window past what the origin agreed.
        if (params.issuedAt == 0 || params.issuedAt > block.timestamp) {
            revert InvalidIssuedAt(params.issuedAt);
        }

        IIntexNFT1155.SeriesData memory seed = IIntexNFT1155.SeriesData({
            issuanceCurrency: params.issuanceCurrency,
            referenceCurrency: params.referenceCurrency,
            issuedUnits: params.issuedUnits,
            promisLoadMinor: params.promisLoadMinor,
            entryPriceMinor: params.entryPriceMinor,
            callPriceMinor: params.callPriceMinor,
            callTrigger: IIntexNFT1155.IntexCallTrigger({
                callWindow: params.callTrigger.callWindow,
                callThreshold: params.callTrigger.callThreshold,
                callNoticePeriod: params.callTrigger.callNoticePeriod
            }),
            issuedAt: params.issuedAt,
            calledAt: 0,
            totalSupply: 0,
            state: IIntexNFT1155.IntexState.Issued,
            worldwideDay: params.worldwideDay,
            seriesId: params.seriesId
        });
        $.seriesData[iTok] = seed;

        // Series remain in allSeries permanently even after supply reaches 0. This
        // preserves the historical record and avoids O(n) removal.
        $.allSeries.push(params.seriesId);
        $.seriesOfDay[params.worldwideDay].push(params.seriesId);
        $.seriesOfPair[params.worldwideDay][params.issuanceCurrency][params.referenceCurrency] = params.seriesId;

        emit MetadataUpdate(iTok);
    }

    /// @inheritdoc IIntexNFT1155
    function issueIntex(address to, uint256 units, bytes14 seriesId) external onlyRole(AUCTION_ROLE) {
        if (to == address(0)) {
            revert ZeroAddress("to", to);
        }

        IntexNFT1155Storage storage $ = _s();
        uint256 tokenId = _issuedTokenId(seriesId);
        IIntexNFT1155.SeriesData storage data = $.seriesData[tokenId];
        if (data.issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }
        if (data.state == IIntexNFT1155.IntexState.Called) {
            uint256 deadline = _settlementDeadline(data);
            if (block.timestamp > deadline) {
                // forge-lint: disable-next-line(unsafe-typecast) -- below block.timestamp, so within uint32
                revert IssueAfterDeadline(tokenId, uint32(deadline));
            }
        }

        // A per-recipient mint is one bidder's auction win, bounded by their bid's
        // `intexQuantity` (uint16). The bound keeps the ERC1155 balance and `totalSupply`
        // consistent.
        if (units > type(uint16).max) revert UnitsTooLarge(units);

        uint256 newTotal = uint256(data.totalSupply) + units;
        if (newTotal > type(uint32).max) {
            revert SupplyCapExceeded(seriesId, newTotal, type(uint32).max);
        }

        // CEI ok: write totalSupply before _mint so the ERC1155 receiver callback observes a
        // consistent (totalSupply == sum balanceOf) snapshot. This closes the read-only-reentrancy
        // window.
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by the uint32 check above
        data.totalSupply = uint32(newTotal);
        _mint(to, tokenId, units, "");

        emit IntexIssued(msg.sender, tokenId, to, units);
    }

    /// @inheritdoc IIntexNFT1155
    function markCalled(bytes14 seriesId, uint32 calledAt) external onlyRole(RELAYER_ROLE) {
        uint256 tokenId = _issuedTokenId(seriesId);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }
        if (data.state != IIntexNFT1155.IntexState.Issued) {
            revert InvalidState(uint8(IIntexNFT1155.IntexState.Issued), uint8(data.state));
        }

        // Zero is the "not called" sentinel, and a future stamp would outlast the origin's window.
        if (calledAt == 0 || calledAt > block.timestamp) revert CalledAtInvalid(calledAt, uint32(block.timestamp));

        IIntexNFT1155.IntexState previousState = data.state;
        data.state = IIntexNFT1155.IntexState.Called;
        data.calledAt = calledAt;

        uint256 deadline = _settlementDeadline(data);
        // forge-lint: disable-next-line(unsafe-typecast) -- saturated to the event's uint32
        uint32 derivedDeadline = deadline > type(uint32).max ? type(uint32).max : uint32(deadline);

        emit IntexStatusUpdated(
            msg.sender, tokenId, previousState, IIntexNFT1155.IntexState.Called, calledAt, derivedDeadline
        );
        emit MetadataUpdate(tokenId);
    }

    /// @inheritdoc IERC1155Bridgeable
    /// @dev Bridge crosschainBurn gating:
    ///      - Series state `Issued`: bridge allowed for `RELAYER_ROLE`
    ///        (voluntary, owner-initiated moves while the series is tradable).
    ///      - Series state `Called`: allowed only until the bridge cutoff before the deadline.
    function crosschainBurn(address from, address to, uint256 tokenId, uint256 amount) external onlyRole(RELAYER_ROLE) {
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) revert NonexistentToken(tokenId);
        if (from == address(0)) revert ZeroAddress("from", from);

        if (data.state == IIntexNFT1155.IntexState.Called) {
            // Past `calledAt + callNoticePeriod` the series is settlement-complete and balances
            // freeze. A hop leaves no later than the cutoff, so its `crosschainMint` has the
            // margin to land before the deadline.
            uint256 cutoff = _bridgeCutoff(data);
            if (block.timestamp > cutoff) {
                // forge-lint: disable-next-line(unsafe-typecast) -- below block.timestamp, so within uint32
                revert BridgeCutoffPassed(tokenId, uint32(cutoff));
            }
        }

        // CEI ok: write before _burn for symmetry with mint. _burn fires no acceptance callback
        // (OZ ERC1155 skips it when to == address(0)), so no read-only-reentrancy surface here.
        // forge-lint: disable-next-line(unsafe-typecast) -- amount <= balance <= totalSupply (uint32); _burn reverts otherwise
        data.totalSupply -= uint32(amount);
        _burn(from, tokenId, amount);
    }

    /// @inheritdoc IERC1155Bridgeable
    /// @dev Bridge crosschainMint mirrors `crosschainBurn`: `RELAYER_ROLE` throughout. Inside the
    ///      call window, the bridge may still mint into a Called series so a bridged balance lands.
    function crosschainMint(address to, uint256 tokenId, uint256 amount) external onlyRole(RELAYER_ROLE) {
        if (to == address(0)) revert ZeroAddress("to", to);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) revert NonexistentToken(tokenId);

        if (data.state == IIntexNFT1155.IntexState.Called) {
            // Mirror of `crosschainBurn`: no bridge-in past the settlement deadline.
            uint256 deadline = _settlementDeadline(data);
            if (block.timestamp > deadline) {
                // forge-lint: disable-next-line(unsafe-typecast) -- below block.timestamp, so within uint32
                revert BridgeAfterDeadline(tokenId, uint32(deadline));
            }
        }

        // A crosschainMinted balance can be an owner's full transferable balance (<= totalSupply, uint32).
        if (amount > type(uint32).max) revert UnitsTooLarge(amount);

        uint256 newTotal = uint256(data.totalSupply) + amount;
        if (newTotal > type(uint32).max) {
            revert SupplyCapExceeded(bytes14(uint112(tokenId)), newTotal, type(uint32).max);
        }

        // CEI ok: write totalSupply before _mint (see issueIntex()).
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by the uint32 check above
        data.totalSupply = uint32(newTotal);
        _mint(to, tokenId, amount, "");
    }

    /// @inheritdoc IIntexNFT1155
    function burnIntex(address owner, bytes14 seriesId, uint256 units) external onlyRole(BURNER_ROLE) {
        if (owner == address(0)) revert ZeroAddress("owner", owner);
        if (units == 0) revert ZeroUnits();

        uint256 tokenId = _issuedTokenId(seriesId);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) revert NonexistentToken(tokenId);

        // forge-lint: disable-next-line(unsafe-typecast) -- units <= balance <= totalSupply (uint32); _burn reverts otherwise
        data.totalSupply -= uint32(units);
        _burn(owner, tokenId, units);
    }

    /// @inheritdoc IIntexNFT1155
    function seriesExists(bytes14 seriesId) external view returns (bool) {
        return _s().seriesData[_issuedTokenId(seriesId)].issuedAt != 0;
    }

    /// @inheritdoc IIntexNFT1155
    function issuedTokenId(bytes14 seriesId) external pure returns (uint256) {
        return _issuedTokenId(seriesId);
    }

    /// @dev Pure helper used internally and exposed via `issuedTokenId`.
    function _issuedTokenId(bytes14 seriesId) private pure returns (uint256) {
        return uint256(uint112(seriesId));
    }

    /// @dev `calledAt + callNoticePeriod`, widened so the sum cannot overflow.
    function _settlementDeadline(IIntexNFT1155.SeriesData storage data) internal view returns (uint256) {
        return uint256(data.calledAt) + data.callTrigger.callNoticePeriod;
    }

    /// @dev Last second a `Called` series may leave a chain: the deadline less a tenth of the notice
    ///      period, capped at `BRIDGE_CUTOFF`.
    function _bridgeCutoff(IIntexNFT1155.SeriesData storage data) internal view returns (uint256) {
        uint256 margin = data.callTrigger.callNoticePeriod / 10;
        if (margin > BRIDGE_CUTOFF) margin = BRIDGE_CUTOFF;
        return _settlementDeadline(data) - margin;
    }

    /// @inheritdoc IIntexNFT1155
    function readData(bytes14 seriesId) external view returns (IIntexNFT1155.SeriesData memory) {
        IntexNFT1155Storage storage $ = _s();
        uint256 tokenId = _issuedTokenId(seriesId);
        // `issuedAt == 0` is the canonical existence sentinel for seriesData entries.
        // slither-disable-next-line incorrect-equality
        if ($.seriesData[tokenId].issuedAt == 0) {
            revert NonexistentToken(tokenId);
        }
        IIntexNFT1155.SeriesData memory data = $.seriesData[tokenId];
        data.state = _effectiveState(data);
        return data;
    }

    /// @dev Derived, never stored: no transaction arrives at the deadline to write it.
    function _effectiveState(IIntexNFT1155.SeriesData memory data) private view returns (IIntexNFT1155.IntexState) {
        if (
            data.state == IIntexNFT1155.IntexState.Called
                && block.timestamp > uint256(data.calledAt) + data.callTrigger.callNoticePeriod
        ) {
            return IIntexNFT1155.IntexState.Forfeited;
        }
        return data.state;
    }

    /// @inheritdoc IIntexNFT1155
    function totalSupply(uint256 tokenId) external view returns (uint256) {
        return _s().seriesData[tokenId].totalSupply;
    }

    /// @inheritdoc IIntexNFT1155
    function uri(uint256 tokenId) public view override(ERC1155Upgradeable, IIntexNFT1155) returns (string memory) {
        IIntexNFT1155.SeriesData memory data = _s().seriesData[tokenId];
        data.state = _effectiveState(data);
        return IntexMetadata.tokenURI(data);
    }

    /// @inheritdoc IIntexNFT1155
    function name() external pure returns (string memory) {
        return "Intex";
    }

    /// @inheritdoc IIntexNFT1155
    function symbol() external pure returns (string memory) {
        return "INTEX";
    }

    /// @inheritdoc IIntexNFT1155
    function contractURI() external pure returns (string memory) {
        return IntexMetadata.contractURI();
    }

    /// @notice ERC1155 transfer hook: freezes Forfeited series.
    /// @dev Transfer lock.
    ///      - Mint/burn paths (from/to address(0)) are always allowed (issue, burnIntex,
    ///        bridge crosschainBurn/crosschainMint).
    ///      - Owner-to-owner transfers stay open until a Called series' settlement deadline.
    ///        Its unpaid units are forfeited then, so they no longer move. Bridge gating is
    ///        separate and lives in `crosschainBurn` / `crosschainMint`.
    /// @param from Sender address (address(0) for mints).
    /// @param to Receiver address (address(0) for burns).
    /// @param ids Array of token IDs.
    /// @param values Array of amounts.
    function _update(address from, address to, uint256[] memory ids, uint256[] memory values) internal override {
        if (from != address(0) && to != address(0)) {
            IntexNFT1155Storage storage $ = _s();
            for (uint256 i = 0; i < ids.length; i++) {
                IIntexNFT1155.SeriesData storage data = $.seriesData[ids[i]];
                if (data.state == IIntexNFT1155.IntexState.Called && block.timestamp > _settlementDeadline(data)) {
                    revert TransferOnForfeitedForbidden(ids[i]);
                }
            }
        }

        super._update(from, to, ids, values);
    }

    // --- Series view functions ---

    /// @inheritdoc IIntexNFT1155
    function totalSeries() external view returns (uint256) {
        return _s().allSeries.length;
    }

    /// @inheritdoc IIntexNFT1155
    function seriesAt(uint256 index) external view returns (bytes14) {
        bytes14[] storage allSeries = _s().allSeries;
        if (index >= allSeries.length) revert SeriesIndexOutOfRange();
        return allSeries[index];
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
        // 0x49064906 = ERC-4906. Literal because OZ's IERC4906 extends IERC721.
        return interfaceId == type(IIntexNFT1155).interfaceId || interfaceId == type(IERC1155Bridgeable).interfaceId
            || interfaceId == bytes4(0x49064906) || super.supportsInterface(interfaceId);
    }
}
