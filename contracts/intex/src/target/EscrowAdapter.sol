// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";
import {IEscrowAdapter, EscrowTimes} from "./interfaces/IEscrowAdapter.sol";
import {ITheCompact} from "../vendor/the-compact/interfaces/ITheCompact.sol";
import {IAllocator} from "../vendor/the-compact/interfaces/IAllocator.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {Scope} from "../vendor/the-compact/types/Scope.sol";
import {ResetPeriod} from "../vendor/the-compact/types/ResetPeriod.sol";

/**
 * @title EscrowAdapter
 * @author Outbe
 * @notice Adapter contract for managing auction bid escrow via The Compact protocol.
 * @dev UUPS upgradeable behind an ERC1967 proxy. The auction and the router are implementation
 *      immutables, fixed at deploy; the asset and The Compact are set by `initialize`.
 *      Integrates with The Compact for fund locking and handles auction finalization.
 *      EscrowAdapter acts as SPONSOR (owns ERC6909 in Compact) and ALLOCATOR (attest).
 *      Both roles bind to the proxy address.
 *      All escrow state is keyed by `emissionDay` (uint32).
 */
contract EscrowAdapter is OwnableUpgradeable, ReentrancyGuardTransient, UUPSUpgradeable, IEscrowAdapter, IAllocator {
    using SafeERC20 for IERC20;

    /// @notice See `EscrowTimes.UNFINALIZED_RETURN_DELAY`.
    uint32 public constant UNFINALIZED_RETURN_DELAY = EscrowTimes.UNFINALIZED_RETURN_DELAY;

    /// @notice Escrow-local safety window on `unlockIntexAuctionBidBondAfterTimeout`, anchored at the bond's
    ///         `lockedAt`. Deliberately time-only (never consults the auction) so a bond survives
    ///         an auction-contract rotation. MUST exceed the auction-side no-reveal gate
    ///         (`revealEnd + UNREVEALED_BOND_LOCK_PERIOD`), at most 3 days after a lock with a day of
    ///         commit and a day of reveal.
    uint32 public constant BID_BOND_ESCROW_TIMEOUT = 4 days;

    /// @notice Escrow-local safety window on `returnIntexAuctionAssetAfterTimeout`, anchored at the lock's
    ///         `lockedAt`. Time-only, so the money survives an auction that no longer works.
    ///         MUST exceed `UNFINALIZED_RETURN_DELAY`.
    uint32 public constant ASSET_ESCROW_TIMEOUT = 4 days;

    /// @dev `reason` of `IntexAuctionAssetReturned`.
    uint8 private constant ASSET_RETURN = 0;
    uint8 private constant ASSET_RETURN_ESCROW_TIMEOUT = 1;

    /// @dev `reason` of `IntexAuctionBidBondUnlocked` for the safety path; the auction gives the other two.
    uint8 private constant BOND_UNLOCK_ESCROW_TIMEOUT = 2;

    /// @dev Locks, bonds and redeems for its bidders: IntexAuction.
    address private immutable INTEX_AUCTION;
    /// @dev Charges the winners and takes the proceeds: IntexAuctionTargetRouter.
    address private immutable INTEX_AUCTION_TARGET_ROUTER;

    /// @custom:storage-location erc7201:outbe.intex.EscrowAdapter
    struct EscrowAdapterStorage {
        /// @dev The Compact contract address.
        ITheCompact compact;
        /// @dev Active payment token used for bid escrow.
        IERC20 asset;
        /// @dev The Compact resource lock ID for our deposits.
        uint256 lockId;
        /// @dev Allocator ID from __registerAllocator.
        uint96 allocatorId;
        /// @dev Lock tag (allocatorId + scope + reset period) for deposits.
        bytes12 lockTag;
        /// @dev Bid locks: emissionDay => bidder => IntexAuctionBidLock.
        mapping(uint32 emissionDay => mapping(address bidder => IntexAuctionBidLock)) bidLocks;
        /// @dev Per-series escrow state.
        mapping(uint32 emissionDay => AuctionEscrowState) auctionEscrowState;
        /// @dev Commit-entry bonds: emissionDay => bidder => IntexAuctionBidBond.
        mapping(uint32 emissionDay => mapping(address bidder => IntexAuctionBidBond)) bidBonds;
        /// @dev Assets rotated away from, keyed by the version they were active under.
        mapping(uint8 version => IntexAuctionAssetVersion) assetVersions;
        /// @dev Version of the active asset held in `compact`, `asset` and `lockId`.
        uint8 currentIntexAuctionAssetVersion;
        /// @dev Clearing terms each day's winners charged at, recorded by its first page with winners.
        mapping(uint32 emissionDay => DayClearing) dayClearing;
        /// @dev Payment-token units in one protocol unit for the active asset.
        uint256 assetScale;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.EscrowAdapter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x9dc6707131c30ec20e38ebcfbc4641faad640e3439439d400ea9dd2fe8f83a00;

    function _s() private pure returns (EscrowAdapterStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address intexAuction_, address intexAuctionTargetRouter_) {
        if (intexAuction_ == address(0)) revert ZeroAddress("intexAuction");
        if (intexAuctionTargetRouter_ == address(0)) revert ZeroAddress("intexAuctionTargetRouter");
        INTEX_AUCTION = intexAuction_;
        INTEX_AUCTION_TARGET_ROUTER = intexAuctionTargetRouter_;
        _disableInitializers();
    }

    /// @notice Initializes the proxy with its owner and the first asset.
    /// @param owner_ The owner, who upgrades the contract and changes the asset.
    /// @param compact_ The Compact contract address.
    /// @param asset_ Payment-token address.
    function initialize(address owner_, address compact_, address asset_) external initializer {
        if (owner_ == address(0)) revert ZeroAddress("owner");

        __Ownable_init(owner_);
        _setIntexAuctionAsset(compact_, asset_);
    }

    /// @dev The owner gates upgrades.
    /// @param newImplementation Address of the implementation the proxy switches to.
    function _authorizeUpgrade(address newImplementation) internal override onlyOwner {}

    modifier onlyFrom(address link) {
        if (msg.sender != link) revert UnauthorizedCaller(msg.sender);
        _;
    }

    // --- Storage getters ---
    /// @notice IntexAuction, the only caller of the lock, bond and redeem entry points.
    function intexAuction() external view returns (address) {
        return INTEX_AUCTION;
    }

    /// @inheritdoc IEscrowAdapter
    function intexAuctionTargetRouter() external view override returns (address) {
        return INTEX_AUCTION_TARGET_ROUTER;
    }

    /// @notice The Compact contract address.
    /// @return The wired Compact instance.
    function compact() external view returns (ITheCompact) {
        return _s().compact;
    }

    /// @notice Active payment token used for bid escrow.
    /// @return The wired payment token.
    function intexAuctionAsset() external view override returns (IERC20) {
        return _s().asset;
    }

    /// @inheritdoc IEscrowAdapter
    function dayIntexAuctionAsset(uint32 emissionDay) external view override returns (IERC20) {
        return _tokenOf(_s().auctionEscrowState[emissionDay].assetVersion);
    }

    /// @notice The Compact resource lock ID for our deposits.
    /// @return The lock id (zero before the first deposit).
    function lockId() external view returns (uint256) {
        return _s().lockId;
    }

    /// @notice Allocator ID from `__registerAllocator`.
    /// @return The registered allocator id (zero before wiring).
    function allocatorId() external view returns (uint96) {
        return _s().allocatorId;
    }

    /// @notice Lock tag (allocatorId + scope + reset period) for deposits.
    /// @return The lock tag derived at allocator registration.
    function lockTag() external view returns (bytes12) {
        return _s().lockTag;
    }

    // --- Admin ---
    /// @inheritdoc IEscrowAdapter
    function setIntexAuctionAsset(address _compact, address _asset) external override onlyOwner {
        _setIntexAuctionAsset(_compact, _asset);
    }

    function _setIntexAuctionAsset(address _compact, address _asset) private {
        if (_compact == address(0)) revert ZeroAddress("compact");
        if (_asset == address(0)) revert ZeroAddress("asset");

        uint8 tokenDecimals = IERC20Metadata(_asset).decimals();
        if (tokenDecimals < BridgeMsgCodec.PROTOCOL_DECIMALS) revert IntexAuctionAssetDecimals(tokenDecimals);

        EscrowAdapterStorage storage $ = _s();

        // A rotation retires the active asset under its version. Locks and bonds taken under it
        // keep withdrawing from it. The first deposit under the new token/Compact bootstraps its
        // own lock.
        bool rotatingPaymentToken = address($.asset) != address(0) && _asset != address($.asset);
        bool rotatingCompact = address($.compact) != address(0) && _compact != address($.compact);
        if (rotatingPaymentToken || rotatingCompact) {
            uint8 retired = $.currentIntexAuctionAssetVersion;
            $.assetVersions[retired] = IntexAuctionAssetVersion({
                compact: address($.compact), asset: $.asset, lockId: $.lockId, assetScale: $.assetScale
            });
            $.currentIntexAuctionAssetVersion = retired + 1;
            emit IntexAuctionAssetVersionRetired(retired, address($.compact), address($.asset), $.lockId);
            $.lockId = 0;
            // A new Compact needs its own allocator registration. Drop the stale
            // allocatorId/lockTag.
            if (rotatingCompact) {
                $.allocatorId = 0;
                $.lockTag = bytes12(0);
            }
        }

        $.compact = ITheCompact(_compact);
        $.asset = IERC20(_asset);
        $.assetScale = 10 ** (tokenDecimals - BridgeMsgCodec.PROTOCOL_DECIMALS);

        $.asset.forceApprove(_compact, 0);
        $.asset.forceApprove(_compact, type(uint256).max);

        // CEI deviation: allocatorId / lockTag depend on __registerAllocator's return. Owner-only,
        // and a re-entrant `compact` is not the owner, so re-entry can't reach here.
        if ($.allocatorId == 0) {
            $.allocatorId = $.compact.__registerAllocator(address(this), "");
            $.lockTag = _buildLockTag($.allocatorId, Scope.ChainSpecific, ResetPeriod.OneMinute);
        }

        emit IntexAuctionAssetSet(_compact, _asset, tokenDecimals);
    }

    // --- IAllocator Implementation ---
    /// @inheritdoc IAllocator
    function attest(address _operator, address _from, address _to, uint256 id, uint256 _amount)
        external
        view
        override
        returns (bytes4)
    {
        if (id != _s().lockId) revert UnexpectedLockId(id);
        return IAllocator.attest.selector;
    }

    /// @inheritdoc IAllocator
    function authorizeClaim(
        bytes32 _claimHash,
        address _arbiter,
        address _sponsor,
        uint256 _nonce,
        uint256 _expires,
        uint256[2][] calldata _idsAndAmounts,
        bytes calldata _allocatorData
    ) external pure override returns (bytes4) {
        revert ClaimAuthorizationUnsupported();
    }

    /// @inheritdoc IAllocator
    function isClaimAuthorized(
        bytes32 _claimHash,
        address _arbiter,
        address _sponsor,
        uint256 _nonce,
        uint256 _expires,
        uint256[2][] calldata _idsAndAmounts,
        bytes calldata _allocatorData
    ) external pure override returns (bool) {
        return false;
    }

    // --- Auction Integration ---
    /// @inheritdoc IEscrowAdapter
    function lockIntexAuctionAsset(uint32 emissionDay, address bidder, uint128 basis, uint32 bidRate, uint16 quantity)
        external
        override
        onlyFrom(INTEX_AUCTION)
        nonReentrant
    {
        if (bidRate == 0) revert ZeroValue("bidRate");
        if (quantity == 0) revert ZeroValue("quantity");
        uint128 amount = _toAssetAmount(_protocolAmount(quantity, basis, bidRate), _s().currentIntexAuctionAssetVersion);
        _validateLockInputs(emissionDay, bidder, amount);
        _lockIntexAuctionAsset(emissionDay, bidder, amount, bidRate, quantity);
    }

    // --- Commit bonds ---
    /// @inheritdoc IEscrowAdapter
    /// @dev Trust boundary: `bidder` is the original `msg.sender` of `IntexAuction.commitIntexAuctionBid`,
    ///      forwarded through the IntexAuction-only entry point (mirrors `lockIntexAuctionAsset`).
    function lockIntexAuctionBidBond(uint32 emissionDay, address bidder, uint128 amount)
        external
        override
        onlyFrom(INTEX_AUCTION)
        nonReentrant
    {
        if (emissionDay == 0) revert ZeroValue("emissionDay");
        if (bidder == address(0)) revert ZeroAddress("bidder");
        if (amount == 0) revert ZeroValue("amount");
        EscrowAdapterStorage storage $ = _s();
        if ($.bidBonds[emissionDay][bidder].amount != 0) revert IntexAuctionBidBondAlreadyLocked();
        amount = _toAssetAmount(amount, $.currentIntexAuctionAssetVersion);

        // CEI deviation mirrors `_lockIntexAuctionAsset`: the one-time lockId bootstrap inside
        // `_depositToCompact` needs depositERC20's return. nonReentrant covers the deviation.
        // slither-disable-next-line arbitrary-send-erc20
        $.asset.safeTransferFrom(bidder, address(this), amount);
        _depositToCompact(amount);

        $.bidBonds[emissionDay][bidder] = IntexAuctionBidBond({
            amount: amount, lockedAt: uint32(block.timestamp), assetVersion: $.currentIntexAuctionAssetVersion
        });
        emit IntexAuctionBidBondLocked(emissionDay, bidder, address($.asset), amount);
    }

    /// @inheritdoc IEscrowAdapter
    function unlockIntexAuctionBidBond(uint32 emissionDay, address bidder, uint8 reason)
        external
        override
        onlyFrom(INTEX_AUCTION)
        nonReentrant
    {
        _unlockIntexAuctionBidBond(emissionDay, bidder, reason);
    }

    /// @inheritdoc IEscrowAdapter
    function unlockIntexAuctionBidBondAfterTimeout(uint32 emissionDay, address bidder) external override nonReentrant {
        IntexAuctionBidBond storage bond = _s().bidBonds[emissionDay][bidder];
        if (bond.amount == 0) revert IntexAuctionBidBondNotFound();
        uint32 unlockableAt = bond.lockedAt + BID_BOND_ESCROW_TIMEOUT;
        if (block.timestamp < unlockableAt) {
            revert IntexAuctionBidBondEscrowTimeoutNotOver(unlockableAt, uint32(block.timestamp));
        }
        _unlockIntexAuctionBidBond(emissionDay, bidder, BOND_UNLOCK_ESCROW_TIMEOUT);
    }

    /// @dev Delete the bond record, withdraw from The Compact, and pay the stored bidder.
    ///      CEI: the delete precedes both external calls. A re-claim reverts `IntexAuctionBidBondNotFound`.
    function _unlockIntexAuctionBidBond(uint32 emissionDay, address bidder, uint8 reason) internal {
        EscrowAdapterStorage storage $ = _s();
        IntexAuctionBidBond memory bond = $.bidBonds[emissionDay][bidder];
        if (bond.amount == 0) revert IntexAuctionBidBondNotFound();
        uint128 amount = bond.amount;

        // Effects
        delete $.bidBonds[emissionDay][bidder];

        // Interactions
        _withdrawFromCompact(bond.assetVersion, amount);
        IERC20 token = _tokenOf(bond.assetVersion);
        token.safeTransfer(bidder, amount);
        emit IntexAuctionBidBondUnlocked(emissionDay, bidder, address(token), amount, reason);
    }

    // --- Bridge Finalization ---
    /// @inheritdoc IEscrowAdapter
    function chargeIntexAuctionAsset(
        uint32 emissionDay,
        address[] calldata winners,
        uint16 partialIndex,
        uint16 partialWon,
        uint64 clearingRate,
        uint128 basis,
        bool completesDay
    ) external override onlyFrom(INTEX_AUCTION_TARGET_ROUTER) nonReentrant returns (uint128 totalPaid) {
        EscrowAdapterStorage storage $ = _s();
        AuctionEscrowState storage state = $.auctionEscrowState[emissionDay];
        if (state.finalized) revert AlreadyFinalized();
        if (completesDay) {
            state.finalized = true;
            state.finalizedAt = uint32(block.timestamp);
        }
        if (partialWon != 0 && partialIndex >= winners.length) {
            revert PartialFillOutsidePage(partialIndex, winners.length);
        }
        if (winners.length != 0) _recordClearing(emissionDay, clearingRate, basis);

        for (uint256 i = 0; i < winners.length; ++i) {
            address bidder = winners[i];
            IntexAuctionBidLock storage lock = $.bidLocks[emissionDay][bidder];
            if (lock.status != LockStatus.Locked) {
                emit IntexAuctionWinNoteCreateFailed(
                    emissionDay, bidder, abi.encodeWithSelector(LockNotActive.selector)
                );
                continue;
            }
            uint16 quantity = lock.quantity;
            if (partialWon != 0 && i == partialIndex) {
                if (partialWon >= quantity) {
                    emit IntexAuctionWinNoteCreateFailed(
                        emissionDay,
                        bidder,
                        abi.encodeWithSelector(PartialFillNotPartial.selector, partialWon, quantity)
                    );
                    continue;
                }
                quantity = partialWon;
            }
            uint128 locked = lock.lockedAmount;
            uint256 charged = _protocolAmount(quantity, basis, clearingRate) * _scaleOf(state.assetVersion);
            if (charged > locked) {
                emit IntexAuctionWinNoteCreateFailed(
                    emissionDay, bidder, abi.encodeWithSelector(ChargeExceedsLock.selector, locked, charged)
                );
                continue;
            }

            // forge-lint: disable-next-line(unsafe-typecast) -- bounded by `locked` just above
            uint128 change = locked - uint128(charged);
            totalPaid += locked - change;
            lock.status = LockStatus.Won;
            lock.quantity = quantity;
            lock.lockedAmount = change;
            address asset = address(_tokenOf(state.assetVersion));
            emit IntexAuctionAssetCharged(emissionDay, bidder, asset, charged);
            emit IntexAuctionWinNoteCreated(emissionDay, bidder, quantity);
        }

        if (totalPaid > 0) state.totalLocked -= totalPaid;

        if (totalPaid > 0) {
            uint8 version = state.assetVersion;
            _withdrawFromCompact(version, totalPaid);
            _tokenOf(version).safeTransfer(INTEX_AUCTION_TARGET_ROUTER, totalPaid);
        }
    }

    /// @dev Record the day's clearing terms on its first chunk with winners, and hold every later
    ///      one to them.
    function _recordClearing(uint32 emissionDay, uint64 clearingRate, uint128 basis) private {
        if (clearingRate == 0 || basis == 0) revert ClearingTermsMissing();
        DayClearing storage clearing = _s().dayClearing[emissionDay];
        if (clearing.basis == 0) {
            clearing.clearingRate = clearingRate;
            clearing.basis = basis;
        } else if (clearing.clearingRate != clearingRate || clearing.basis != basis) {
            revert ClearingTermsMismatch(clearingRate, basis);
        }
    }

    /// @inheritdoc IEscrowAdapter
    function returnIntexAuctionAsset(uint32 emissionDay, address bidder)
        external
        override
        onlyFrom(INTEX_AUCTION)
        nonReentrant
        returns (bool closed, bool finalized)
    {
        if (bidder == address(0)) revert ZeroAddress("bidder");

        EscrowAdapterStorage storage $ = _s();
        IntexAuctionBidLock storage lock = $.bidLocks[emissionDay][bidder];
        AuctionEscrowState storage state = $.auctionEscrowState[emissionDay];
        finalized = state.finalized;
        LockStatus status = lock.status;
        if (status == LockStatus.None) {
            // The escrow-local valve already paid it out.
            if (!finalized) revert LockNotActive();
            return (true, true);
        }

        (uint128 returnedAmount, uint32 returnableAt) = _returnable(state, lock);
        if (block.timestamp < returnableAt) {
            revert IntexAuctionAssetNotYetReturnable(returnableAt, uint32(block.timestamp));
        }
        closed = _release(emissionDay, state, lock, bidder, returnedAmount, ASSET_RETURN);
    }

    /// @inheritdoc IEscrowAdapter
    function returnIntexAuctionAssetAfterTimeout(uint32 emissionDay, address bidder) external override nonReentrant {
        EscrowAdapterStorage storage $ = _s();
        IntexAuctionBidLock storage lock = $.bidLocks[emissionDay][bidder];
        if (lock.status == LockStatus.None) revert LockNotActive();
        uint32 returnableAt = lock.lockedAt + ASSET_ESCROW_TIMEOUT;
        if (block.timestamp < returnableAt) {
            revert IntexAuctionAssetEscrowTimeoutNotOver(returnableAt, uint32(block.timestamp));
        }
        _release(
            emissionDay, $.auctionEscrowState[emissionDay], lock, bidder, lock.lockedAmount, ASSET_RETURN_ESCROW_TIMEOUT
        );
    }

    /// @dev Pay `returnedAmount` out of the lock. A winner's lock keeps its units, any other lock is deleted.
    function _release(
        uint32 emissionDay,
        AuctionEscrowState storage state,
        IntexAuctionBidLock storage lock,
        address bidder,
        uint128 returnedAmount,
        uint8 reason
    ) private returns (bool closed) {
        if (lock.status == LockStatus.Won) {
            if (returnedAmount == 0) revert NothingToReturn();
            lock.lockedAmount = 0;
        } else {
            delete _s().bidLocks[emissionDay][bidder];
            closed = true;
        }
        _returnAsset(emissionDay, state, bidder, returnedAmount, reason);
    }

    /// @inheritdoc IEscrowAdapter
    function redeemWinNote(uint32 emissionDay, address bidder)
        external
        override
        onlyFrom(INTEX_AUCTION)
        nonReentrant
        returns (uint16 units)
    {
        EscrowAdapterStorage storage $ = _s();
        IntexAuctionBidLock storage lock = $.bidLocks[emissionDay][bidder];
        if (lock.status != LockStatus.Won) revert LockNotActive();
        units = lock.quantity;
        uint128 change = lock.lockedAmount;
        delete $.bidLocks[emissionDay][bidder];
        if (change > 0) _returnAsset(emissionDay, $.auctionEscrowState[emissionDay], bidder, change, ASSET_RETURN);
    }

    /// @dev Withdraw `amount` of the day's escrow from The Compact and pay it to `bidder`.
    function _returnAsset(
        uint32 emissionDay,
        AuctionEscrowState storage state,
        address bidder,
        uint128 amount,
        uint8 reason
    ) private {
        // A winner's payment already left with the day's proceeds, so this is all the lock held.
        state.totalLocked -= amount;
        uint8 version = state.assetVersion;
        _withdrawFromCompact(version, amount);
        IERC20 token = _tokenOf(version);
        token.safeTransfer(bidder, amount);
        emit IntexAuctionAssetReturned(emissionDay, bidder, address(token), amount, reason);
    }

    /// @dev What a live lock is owed and from when. A winner is owed its change at once.
    ///      A bidder the finalized day never named lost and is owed its principal at once. A day
    ///      that never finalized owes the principal after the delay.
    function _returnable(AuctionEscrowState storage state, IntexAuctionBidLock storage lock)
        private
        view
        returns (uint128 returnedAmount, uint32 returnableAt)
    {
        if (lock.status == LockStatus.Won) return (lock.lockedAmount, 0);
        if (!state.finalized) return (lock.lockedAmount, lock.lockedAt + UNFINALIZED_RETURN_DELAY);
        return (lock.lockedAmount, 0);
    }

    // --- Views ---
    /// @inheritdoc IEscrowAdapter
    function getIntexAuctionBidLock(uint32 emissionDay, address bidder)
        external
        view
        override
        returns (IntexAuctionBidLock memory)
    {
        return _s().bidLocks[emissionDay][bidder];
    }

    /// @inheritdoc IEscrowAdapter
    function returnableAmount(uint32 emissionDay, address bidder)
        external
        view
        override
        returns (uint128 amount, uint32 returnableAt)
    {
        EscrowAdapterStorage storage $ = _s();
        IntexAuctionBidLock storage lock = $.bidLocks[emissionDay][bidder];
        if (lock.status != LockStatus.Locked && lock.status != LockStatus.Won) return (0, 0);
        return _returnable($.auctionEscrowState[emissionDay], lock);
    }

    /// @inheritdoc IEscrowAdapter
    function getIntexAuctionBidBond(uint32 emissionDay, address bidder)
        external
        view
        override
        returns (IntexAuctionBidBond memory)
    {
        return _s().bidBonds[emissionDay][bidder];
    }

    /// @inheritdoc IEscrowAdapter
    function auctionStatus(uint32 emissionDay)
        external
        view
        override
        returns (bool hasLocks, bool isFinalized, uint128 totalLocked)
    {
        AuctionEscrowState memory state = _s().auctionEscrowState[emissionDay];
        return (state.lockCount > 0, state.finalized, state.totalLocked);
    }

    /// @inheritdoc IEscrowAdapter
    function currentIntexAuctionAssetVersion() external view override returns (uint8) {
        return _s().currentIntexAuctionAssetVersion;
    }

    /// @inheritdoc IEscrowAdapter
    function getIntexAuctionAssetVersion(uint8 version)
        external
        view
        override
        returns (IntexAuctionAssetVersion memory)
    {
        EscrowAdapterStorage storage $ = _s();
        if (version == $.currentIntexAuctionAssetVersion) {
            return IntexAuctionAssetVersion({
                compact: address($.compact), asset: $.asset, lockId: $.lockId, assetScale: $.assetScale
            });
        }
        return $.assetVersions[version];
    }

    // --- Internal helpers ---
    /// @notice Validate lock inputs before any state write.
    /// @dev Rejects a zero `emissionDay`, zero `bidder`, zero `amount`, and a bidder that already
    ///      holds a non-`None` lock for the series.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @param amount Amount to lock.
    function _validateLockInputs(uint32 emissionDay, address bidder, uint128 amount) internal view {
        // Cheap sanity floor: the IntexAuction-only gate already guarantees a real, stage-gated series.
        // But a zero id is obviously bogus, so this check rejects it before any state write.
        if (emissionDay == 0) revert ZeroValue("emissionDay");
        if (bidder == address(0)) revert ZeroAddress("bidder");
        if (amount == 0) revert ZeroValue("amount");
        if (_s().bidLocks[emissionDay][bidder].status != LockStatus.None) {
            revert IntexAuctionBidAlreadyLocked();
        }
    }

    /// @notice Execute the lock operation - transfer from the bidder and deposit to The Compact.
    /// @dev Bootstraps `lockId` and forced withdrawal on the first deposit, then records the
    ///      `IntexAuctionBidLock` and bumps the per-series escrow stats.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @param amount Amount to lock.
    /// @dev Trust boundary: `bidder` is the original `msg.sender` of `IntexAuction.revealIntexAuctionBid`,
    ///      forwarded through `lockIntexAuctionAsset`, which only IntexAuction may call.
    function _lockIntexAuctionAsset(uint32 emissionDay, address bidder, uint128 amount, uint32 bidRate, uint16 quantity)
        internal
    {
        EscrowAdapterStorage storage $ = _s();
        AuctionEscrowState storage state = $.auctionEscrowState[emissionDay];
        // A day's locks share one asset, so its proceeds leave in one withdrawal.
        uint8 version = $.currentIntexAuctionAssetVersion;
        if (state.lockCount != 0 && state.assetVersion != version) {
            revert DayIntexAuctionAssetRetired(emissionDay, state.assetVersion);
        }

        // CEI deviation: only the one-time lockId bootstrap needs depositERC20's return before
        // writing. Per-call bidLocks / auctionEscrowState writes follow for locality and could
        // move above. nonReentrant on every outer entrypoint covers the deviation regardless.
        // slither-disable-next-line arbitrary-send-erc20
        $.asset.safeTransferFrom(bidder, address(this), amount);
        _depositToCompact(amount);

        // Store lock data.
        $.bidLocks[emissionDay][bidder] = IntexAuctionBidLock({
            lockedAmount: amount,
            lockedAt: uint32(block.timestamp),
            status: LockStatus.Locked,
            bidRate: bidRate,
            quantity: quantity
        });

        // Update series escrow stats.
        state.assetVersion = version;
        ++state.lockCount;
        state.totalLocked += amount;

        emit IntexAuctionAssetLocked(emissionDay, bidder, address($.asset), amount);
    }

    /// @notice Deposit `amount` of the payment token into The Compact (we receive ERC6909 tokens).
    /// @dev Bootstraps `lockId` and enables forced withdrawal on the first-ever deposit. The
    ///      returned `withdrawableAt` is informational. Withdrawals invoke forcedWithdrawal
    ///      directly.
    /// @param amount Amount to deposit.
    function _depositToCompact(uint128 amount) internal {
        EscrowAdapterStorage storage $ = _s();
        uint256 returnedLockId = $.compact.depositERC20(address($.asset), $.lockTag, amount, address(this));
        if ($.lockId == 0) {
            $.lockId = returnedLockId;
            // slither-disable-next-line unused-return
            $.compact.enableForcedWithdrawal(returnedLockId);
        }
    }

    /// @notice Withdraw tokens from The Compact via forced withdrawal, out of the position
    ///         `version` refers to.
    /// @dev Reverts `NoDeposits` if that position never bootstrapped and `ForcedWithdrawalFailed`
    ///      if the reset period has not elapsed (The Compact returns false).
    /// @param version Asset version the funds were deposited under.
    /// @param amount Amount to withdraw.
    function _withdrawFromCompact(uint8 version, uint128 amount) internal {
        EscrowAdapterStorage storage $ = _s();
        (ITheCompact compactOf, uint256 idOf) = version == $.currentIntexAuctionAssetVersion
            ? ($.compact, $.lockId)
            : (ITheCompact($.assetVersions[version].compact), $.assetVersions[version].lockId);
        if (idOf == 0) revert NoDeposits();
        bool success = compactOf.forcedWithdrawal(idOf, address(this), amount);
        if (!success) revert ForcedWithdrawalFailed();
    }

    /// @dev Payment token of the asset `version` refers to.
    function _tokenOf(uint8 version) internal view returns (IERC20) {
        EscrowAdapterStorage storage $ = _s();
        return version == $.currentIntexAuctionAssetVersion ? $.asset : $.assetVersions[version].asset;
    }

    /// @dev Payment-token units in one protocol unit for the asset `version` refers to.
    function _scaleOf(uint8 version) internal view returns (uint256) {
        EscrowAdapterStorage storage $ = _s();
        return version == $.currentIntexAuctionAssetVersion ? $.assetScale : $.assetVersions[version].assetScale;
    }

    /// @dev `quantity` Intexes at `rate` of the protocol `basis`: the lock a bid takes and the
    ///      payment a winner makes. Floored at six decimals, like the clearing side's `rate_lock`.
    function _protocolAmount(uint256 quantity, uint256 basis, uint256 rate) private pure returns (uint256) {
        return quantity * basis * rate / BridgeMsgCodec.SCALE_1E6;
    }

    /// @dev A protocol `amount` in units of the asset `version` refers to.
    function _toAssetAmount(uint256 amount, uint8 version) internal view returns (uint128) {
        uint256 scaled = amount * _scaleOf(version);
        if (scaled > type(uint128).max) revert IntexAuctionAssetAmountOverflow(scaled);
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded just above
        return uint128(scaled);
    }

    /// @dev Build the lock tag for The Compact deposits.
    /// @notice Combines allocatorId, scope, and reset period into a single 12-byte identifier.
    ///         The Compact uses this tag to identify which resource lock to use for deposits.
    /// @param _allocatorId Our allocator ID from The Compact registration.
    /// @param scope Whether the lock is multichain or chain-specific.
    /// @param resetPeriod Time period before forced withdrawal is allowed.
    /// @return lockTag 12-byte identifier used for all deposits.
    function _buildLockTag(uint96 _allocatorId, Scope scope, ResetPeriod resetPeriod) internal pure returns (bytes12) {
        uint256 packed =
            (uint256(uint8(scope)) << 255) | (uint256(uint8(resetPeriod)) << 252) | (uint256(_allocatorId) << 160);
        // forge-lint: disable-next-line(unsafe-typecast) -- intentional truncation to the tag's top 12 bytes
        return bytes12(uint96(packed >> 160));
    }

    /// @notice Check if the contract supports a given interface.
    /// @dev Returns true for `IAllocator` and ERC-165.
    /// @param interfaceId Interface ID to check.
    /// @return True if the interface is supported.
    function supportsInterface(bytes4 interfaceId) public pure returns (bool) {
        return interfaceId == type(IAllocator).interfaceId || interfaceId == type(IERC165).interfaceId;
    }
}
