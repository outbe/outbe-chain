// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1155Upgradeable} from "@openzeppelin/contracts-upgradeable/token/ERC1155/ERC1155Upgradeable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
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
 * @dev UUPS upgradeable behind an ERC1967 proxy. Every contract that may write here is an
 *      implementation immutable, fixed at deploy.
 * @dev One auction produces one series with shared parameters for all winners.
 * @dev State transitions affect the entire series simultaneously (O(1) gas).
 * @dev Series lifecycle: Issued -> Called.
 *      Forfeit is not an on-chain state. It is derived against the clock once the transit margin
 *      past `calledAt + callNoticePeriod` ends (issue/bridge gates, metadata rendering).
 * @dev A series' units are the token id `uint256(keccak256(seriesId))`. Settled units live in the Intex
 *      precompile on Outbe: a settlement burns them here.
 */
contract IntexNFT1155 is ERC1155Upgradeable, OwnableUpgradeable, UUPSUpgradeable, IIntexNFT1155 {
    /// @dev Opens and calls the series: IntexAuctionTargetRouter.
    address private immutable TARGET_ROUTER;
    /// @dev Burns and mints the units that cross chains: IntexNFT1155Router.
    address private immutable INTEX_NFT1155_ROUTER;
    /// @dev Issues the units of a winner: IntexAuction.
    address private immutable INTEX_AUCTION;
    /// @dev Burns the settled units: the IntexFactory precompile on Outbe, zero elsewhere.
    address private immutable INTEX_FACTORY;
    /// @dev Moves units into a Gem Factory position: the GemFactory precompile on Outbe, zero elsewhere.
    address private immutable GEM_FACTORY;

    /// @custom:storage-location erc7201:outbe.intex.IntexNFT1155
    struct IntexNFT1155Storage {
        /// @dev Series-level data, stored once per series under its token id.
        mapping(uint256 intexId => IIntexNFT1155.SeriesData) seriesData;
        /// @dev Every series ever created on this chain, in creation order.
        bytes14[] allSeries;
        /// @dev Series ids issued per emission day.
        mapping(uint32 emissionDay => bytes14[] seriesIds) seriesOfDay;
        /// @dev The series a day's currency pair was created under.
        mapping(
            uint32 emissionDay
                => mapping(uint16 issuanceCurrency => mapping(uint16 referenceCurrency => bytes14 seriesId))
        ) seriesOfPair;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexNFT1155")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0xe941cbaf65abb9f7003c3006add9c5d12ba7e339abdf88d4afd5defeb8932900;

    function _s() private pure returns (IntexNFT1155Storage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(
        address targetRouter_,
        address intexNFT1155Router_,
        address intexAuction_,
        address intexFactory_,
        address gemFactory_
    ) {
        if (targetRouter_ == address(0)) revert ZeroAddress("targetRouter", targetRouter_);
        if (intexNFT1155Router_ == address(0)) revert ZeroAddress("intexNFT1155Router", intexNFT1155Router_);
        if (intexAuction_ == address(0)) revert ZeroAddress("intexAuction", intexAuction_);
        TARGET_ROUTER = targetRouter_;
        INTEX_NFT1155_ROUTER = intexNFT1155Router_;
        INTEX_AUCTION = intexAuction_;
        INTEX_FACTORY = intexFactory_;
        GEM_FACTORY = gemFactory_;
        _disableInitializers();
    }

    /// @notice Initializes the proxy with its owner.
    /// @param owner_ The owner, who upgrades the contract.
    function initialize(address owner_) external initializer {
        if (owner_ == address(0)) revert ZeroAddress("owner", owner_);

        __ERC1155_init("");
        __Ownable_init(owner_);
    }

    /// @dev The owner gates upgrades.
    /// @param newImplementation Address of the implementation the proxy switches to.
    function _authorizeUpgrade(address newImplementation) internal override onlyOwner {}

    modifier onlyFrom(address link) {
        if (msg.sender != link) revert UnauthorizedCaller(msg.sender);
        _;
    }

    /// @inheritdoc IIntexNFT1155
    function targetRouter() external view returns (address) {
        return TARGET_ROUTER;
    }

    /// @inheritdoc IIntexNFT1155
    function intexNFT1155Router() external view returns (address) {
        return INTEX_NFT1155_ROUTER;
    }

    /// @inheritdoc IIntexNFT1155
    function intexAuction() external view returns (address) {
        return INTEX_AUCTION;
    }

    /// @inheritdoc IIntexNFT1155
    function intexFactory() external view returns (address) {
        return INTEX_FACTORY;
    }

    /// @inheritdoc IIntexNFT1155
    function gemFactory() external view returns (address) {
        return GEM_FACTORY;
    }

    /// @inheritdoc IIntexNFT1155
    function emissionDayOf(bytes14 seriesId) external view returns (uint32) {
        return _s().seriesData[_issuedIntexId(seriesId)].emissionDay;
    }

    /// @inheritdoc IIntexNFT1155
    function seriesIdsByEmissionDay(uint32 emissionDay) external view returns (bytes14[] memory) {
        return _s().seriesOfDay[emissionDay];
    }

    /// @inheritdoc IIntexNFT1155
    function seriesIdOf(uint32 emissionDay, uint16 issuanceCurrency, uint16 referenceCurrency)
        external
        view
        returns (bytes14)
    {
        return _s().seriesOfPair[emissionDay][issuanceCurrency][referenceCurrency];
    }

    /// @inheritdoc IIntexNFT1155
    function openSeries(IIntexNFT1155.OpenSeriesParams calldata params) external onlyFrom(TARGET_ROUTER) {
        IntexNFT1155Storage storage $ = _s();
        uint256 intexId = _issuedIntexId(params.seriesId);

        if ($.seriesData[intexId].issuedAt != 0) {
            revert IntexAlreadyExists(intexId);
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
            seriesId: params.seriesId,
            callTrigger: IIntexNFT1155.IntexCallTrigger({
                callWindow: params.callTrigger.callWindow,
                callThreshold: params.callTrigger.callThreshold,
                callNoticePeriod: params.callTrigger.callNoticePeriod,
                transitMargin: params.callTrigger.transitMargin
            }),
            issuedAt: params.issuedAt,
            calledAt: 0,
            totalSupply: 0,
            state: IIntexNFT1155.IntexState.Issued,
            emissionDay: params.emissionDay
        });
        $.seriesData[intexId] = seed;

        // Series remain in allSeries permanently even after supply reaches 0. This
        // preserves the historical record and avoids O(n) removal.
        $.allSeries.push(params.seriesId);
        $.seriesOfDay[params.emissionDay].push(params.seriesId);
        $.seriesOfPair[params.emissionDay][params.issuanceCurrency][params.referenceCurrency] = params.seriesId;

        emit IntexSeriesOpened(
            params.seriesId,
            params.issuanceCurrency,
            params.referenceCurrency,
            params.entryPriceMinor,
            params.callPriceMinor,
            params.callTrigger.callWindow,
            params.callTrigger.callThreshold,
            params.callTrigger.callNoticePeriod,
            params.callTrigger.transitMargin,
            params.issuedUnits,
            params.promisLoadMinor
        );
        emit MetadataUpdate(intexId);
    }

    /// @inheritdoc IIntexNFT1155
    function issueIntex(address to, uint256 units, bytes14 seriesId) external onlyFrom(INTEX_AUCTION) {
        if (to == address(0)) {
            revert ZeroAddress("to", to);
        }

        IntexNFT1155Storage storage $ = _s();
        uint256 intexId = _issuedIntexId(seriesId);
        IIntexNFT1155.SeriesData storage data = $.seriesData[intexId];
        if (data.issuedAt == 0) {
            revert IntexNotFound(intexId);
        }
        if (data.state == IIntexNFT1155.IntexState.Called) {
            uint256 closeAt = _transitCloseAt(data);
            if (block.timestamp > closeAt) {
                // forge-lint: disable-next-line(unsafe-typecast) -- below block.timestamp, so within uint64
                revert IssueAfterDeadline(intexId, uint64(closeAt));
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
        _mint(to, intexId, units, "");

        bytes memory payload = abi.encode(uint256(data.promisLoadMinor) * units);
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by the uint16 check above
        emit IntexIssued(intexId, to, seriesId, data.issuanceCurrency, data.referenceCurrency, uint32(units), payload);
    }

    /// @inheritdoc IIntexNFT1155
    function callSeries(bytes14 seriesId, uint64 calledAt) external onlyFrom(TARGET_ROUTER) {
        uint256 intexId = _issuedIntexId(seriesId);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[intexId];
        if (data.issuedAt == 0) {
            revert IntexNotFound(intexId);
        }
        if (data.state != IIntexNFT1155.IntexState.Issued) {
            revert InvalidState(uint8(IIntexNFT1155.IntexState.Issued), uint8(data.state));
        }

        // Zero is the "not called" sentinel, and a future stamp would outlast the origin's window.
        if (calledAt == 0 || calledAt > block.timestamp) revert CalledAtInvalid(calledAt, uint64(block.timestamp));

        data.state = IIntexNFT1155.IntexState.Called;
        data.calledAt = calledAt;

        uint256 deadline = _settlementDeadline(data);
        // forge-lint: disable-next-line(unsafe-typecast) -- saturated to the event's uint64
        uint64 derivedDeadline = deadline > type(uint64).max ? type(uint64).max : uint64(deadline);

        emit IntexSeriesCalled(seriesId, data.issuanceCurrency, data.referenceCurrency, calledAt, derivedDeadline);
        emit MetadataUpdate(intexId);
    }

    /// @inheritdoc IERC1155Bridgeable
    /// @dev Bridge crosschainBurn gating:
    ///      - Series state `Issued`: allowed (voluntary, owner-initiated moves while the series is tradable).
    ///      - Series state `Called`: allowed to any recipient until the settlement deadline.
    function crosschainBurn(address from, address to, uint256 tokenId, uint256 amount)
        external
        onlyFrom(INTEX_NFT1155_ROUTER)
    {
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) revert IntexNotFound(tokenId);
        if (from == address(0)) revert ZeroAddress("from", from);

        if (data.state == IIntexNFT1155.IntexState.Called) {
            // A hop leaves by the deadline, and the transit margin past it lets the hop land and settle.
            uint256 deadline = _settlementDeadline(data);
            if (block.timestamp > deadline) {
                // forge-lint: disable-next-line(unsafe-typecast) -- below block.timestamp, so within uint64
                revert BridgeAfterDeadline(tokenId, uint64(deadline));
            }
        }

        // CEI ok: write before _burn for symmetry with mint. _burn fires no acceptance callback
        // (OZ ERC1155 skips it when to == address(0)), so no read-only-reentrancy surface here.
        // forge-lint: disable-next-line(unsafe-typecast) -- amount <= balance <= totalSupply (uint32); _burn reverts otherwise
        data.totalSupply -= uint32(amount);
        _burn(from, tokenId, amount);
    }

    /// @inheritdoc IERC1155Bridgeable
    /// @dev Bridge crosschainMint mirrors `crosschainBurn`: IntexNFT1155Router only. Until the transit
    ///      margin past the deadline ends, the bridge may still mint into a Called series so a hop lands.
    function crosschainMint(address to, uint256 tokenId, uint256 amount) external onlyFrom(INTEX_NFT1155_ROUTER) {
        if (to == address(0)) revert ZeroAddress("to", to);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[tokenId];
        if (data.issuedAt == 0) revert IntexNotFound(tokenId);

        if (data.state == IIntexNFT1155.IntexState.Called) {
            uint256 closeAt = _transitCloseAt(data);
            if (block.timestamp > closeAt) {
                // forge-lint: disable-next-line(unsafe-typecast) -- below block.timestamp, so within uint64
                revert BridgeAfterDeadline(tokenId, uint64(closeAt));
            }
        }

        // A crosschainMinted balance can be an owner's full transferable balance (<= totalSupply, uint32).
        if (amount > type(uint32).max) revert UnitsTooLarge(amount);

        uint256 newTotal = uint256(data.totalSupply) + amount;
        if (newTotal > type(uint32).max) {
            revert SupplyCapExceeded(data.seriesId, newTotal, type(uint32).max);
        }

        // CEI ok: write totalSupply before _mint (see issueIntex()).
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by the uint32 check above
        data.totalSupply = uint32(newTotal);
        _mint(to, tokenId, amount, "");
    }

    /// @inheritdoc IIntexNFT1155
    function burnIntex(address owner, bytes14 seriesId, uint256 units) external onlyFrom(INTEX_FACTORY) {
        _burnIssued(owner, seriesId, units);
    }

    /// @inheritdoc IIntexNFT1155
    function sendIntexToGemFactory(address owner, bytes14 seriesId, uint256 units, uint256 gemFactoryPositionId)
        external
        onlyFrom(GEM_FACTORY)
    {
        uint256 intexId = _burnIssued(owner, seriesId, units);
        // forge-lint: disable-next-line(unsafe-typecast) -- _burnIssued bounded units by the uint32 supply
        emit IntexSentToGemFactory(intexId, owner, gemFactoryPositionId, uint32(units));
    }

    function _burnIssued(address owner, bytes14 seriesId, uint256 units) private returns (uint256 intexId) {
        if (owner == address(0)) revert ZeroAddress("owner", owner);
        if (units == 0) revert ZeroUnits();

        intexId = _issuedIntexId(seriesId);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[intexId];
        if (data.issuedAt == 0) revert IntexNotFound(intexId);

        // forge-lint: disable-next-line(unsafe-typecast) -- units <= balance <= totalSupply (uint32); _burn reverts otherwise
        data.totalSupply -= uint32(units);
        _burn(owner, intexId, units);
    }

    /// @inheritdoc IIntexNFT1155
    function burnForfeited(address owner, bytes14 seriesId) external {
        uint256 intexId = _issuedIntexId(seriesId);
        IIntexNFT1155.SeriesData storage data = _s().seriesData[intexId];
        if (data.issuedAt == 0) revert IntexNotFound(intexId);
        IIntexNFT1155.IntexState state = _effectiveState(data);
        if (state != IIntexNFT1155.IntexState.Forfeited) {
            revert InvalidState(uint8(IIntexNFT1155.IntexState.Forfeited), uint8(state));
        }
        uint256 units = balanceOf(owner, intexId);
        if (units == 0) revert ZeroUnits();

        // forge-lint: disable-next-line(unsafe-typecast) -- units <= totalSupply (uint32)
        uint32 burned = uint32(units);
        data.totalSupply -= burned;
        _burn(owner, intexId, units);
        emit IntexForfeited(intexId, owner, data.issuanceCurrency, data.referenceCurrency, burned);
    }

    /// @inheritdoc IIntexNFT1155
    function seriesExists(bytes14 seriesId) external view returns (bool) {
        return _s().seriesData[_issuedIntexId(seriesId)].issuedAt != 0;
    }

    /// @inheritdoc IIntexNFT1155
    function issuedIntexId(bytes14 seriesId) external pure returns (uint256) {
        return _issuedIntexId(seriesId);
    }

    /// @dev Pure helper used internally and exposed via `issuedIntexId`.
    function _issuedIntexId(bytes14 seriesId) private pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked(seriesId)));
    }

    /// @dev `calledAt + callNoticePeriod`, widened so the sum cannot overflow.
    function _settlementDeadline(IIntexNFT1155.SeriesData storage data) internal view returns (uint256) {
        return uint256(data.calledAt) + data.callTrigger.callNoticePeriod;
    }

    /// @dev Last second a `Called` unit may still land and settle: the deadline plus the transit margin.
    function _transitCloseAt(IIntexNFT1155.SeriesData storage data) internal view returns (uint256) {
        return _settlementDeadline(data) + data.callTrigger.transitMargin;
    }

    /// @inheritdoc IIntexNFT1155
    function getIntexSeries(bytes14 seriesId) external view returns (IIntexNFT1155.SeriesData memory) {
        return getIntex(_issuedIntexId(seriesId));
    }

    /// @inheritdoc IIntexNFT1155
    function getIntex(uint256 intexId) public view returns (IIntexNFT1155.SeriesData memory) {
        IntexNFT1155Storage storage $ = _s();
        // `issuedAt == 0` is the canonical existence sentinel for seriesData entries.
        // slither-disable-next-line incorrect-equality
        if ($.seriesData[intexId].issuedAt == 0) {
            revert IntexNotFound(intexId);
        }
        IIntexNFT1155.SeriesData memory data = $.seriesData[intexId];
        data.state = _effectiveState(data);
        return data;
    }

    /// @dev Derived, never stored: no transaction arrives at the transit close to write it.
    function _effectiveState(IIntexNFT1155.SeriesData memory data) private view returns (IIntexNFT1155.IntexState) {
        if (
            data.state == IIntexNFT1155.IntexState.Called
                && block.timestamp
                    > uint256(data.calledAt) + data.callTrigger.callNoticePeriod + data.callTrigger.transitMargin
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
    ///      - Owner-to-owner transfers stay open until a Called series' settlement deadline, the
    ///        same moment its units stop leaving by bridge. Bridge gating is separate and lives in
    ///        `crosschainBurn` / `crosschainMint`.
    /// @param from Sender address (address(0) for mints).
    /// @param to Receiver address (address(0) for burns).
    /// @param ids Array of token IDs.
    /// @param values Array of amounts.
    function _update(address from, address to, uint256[] memory ids, uint256[] memory values) internal override {
        if (from != address(0) && to != address(0)) {
            IntexNFT1155Storage storage $ = _s();
            for (uint256 i = 0; i < ids.length; i++) {
                IIntexNFT1155.SeriesData storage data = $.seriesData[ids[i]];
                if (data.state == IIntexNFT1155.IntexState.Called) {
                    uint256 deadline = _settlementDeadline(data);
                    if (block.timestamp > deadline) {
                        // forge-lint: disable-next-line(unsafe-typecast) -- below block.timestamp, so within uint64
                        revert TransferAfterDeadline(ids[i], uint64(deadline));
                    }
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
    ///      interfaces advertised by ERC1155.
    /// @param interfaceId The ERC-165 interface identifier to query.
    /// @return True if the contract implements `interfaceId`.
    function supportsInterface(bytes4 interfaceId) public view override(IERC165, ERC1155Upgradeable) returns (bool) {
        // 0x49064906 = ERC-4906. Literal because OZ's IERC4906 extends IERC721.
        return interfaceId == type(IIntexNFT1155).interfaceId || interfaceId == type(IERC1155Bridgeable).interfaceId
            || interfaceId == bytes4(0x49064906) || super.supportsInterface(interfaceId);
    }
}
