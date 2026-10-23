// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";

import {IAuction} from "./interfaces/IAuction.sol";
import {OrderValidator} from "./libs/OrderValidator.sol";

/// @title Auction
/// @notice Commit-reveal Vickrey auction for competitive solver selection
/// @dev Phases: commit (10s) -> reveal (10s) -> getWinner. The window of an order is fixed when it starts.
///      Winner = highest outputAmount, pays second-highest price.
contract Auction is IAuction, Ownable2Step {
    // ============ Phase Constants ============

    uint8 internal constant PHASE_NONE = 0;
    uint8 internal constant PHASE_COMMIT = 1;
    uint8 internal constant PHASE_REVEAL = 2;
    uint8 internal constant PHASE_ENDED = 3;

    /// @notice Upper bound of maxQuotesPerOrder. getWinner scans every quote on the settlement path.
    uint256 public constant MAX_QUOTES_PER_ORDER = 50;

    // ============ Storage ============

    /// @notice Duration of the commit phase in seconds
    uint256 public commitPeriod = 10 seconds;

    /// @notice Duration of the reveal phase in seconds
    uint256 public revealPeriod = 10 seconds;

    /// @notice Maximum number of revealed quotes allowed per order
    uint256 public maxQuotesPerOrder = 10;

    /// @notice Router contract - authorized caller
    address public router;

    /// @notice Revealed quotes per order
    mapping(bytes32 => Quote[]) internal _quotes;

    /// @notice Auction window per order. commit and resetAuction set it, so a later period change
    ///         applies only to auctions that start after it (startedAt 0 = no auction).
    mapping(bytes32 => Window) internal _window;

    /// @notice Per-order auction epoch. Every resetAuction increments it, so prior-round commits
    ///         become unreachable without enumerating the (non-enumerable) _commits mapping.
    mapping(bytes32 => uint256) internal _auctionEpoch;

    /// @notice Commitment hashes: orderId => epoch => solver => commitHash
    mapping(bytes32 => mapping(uint256 => mapping(address => bytes32))) internal _commits;

    // ============ Modifiers ============

    modifier onlyRouter() {
        _onlyRouter();
        _;
    }

    function _onlyRouter() internal view {
        if (msg.sender != router) revert UnauthorizedRouter();
    }

    // ============ Constructor ============

    constructor(address _owner) Ownable(_owner) {}

    // ============ Commit-Reveal Functions ============

    /// @inheritdoc IAuction
    function commit(bytes32 orderId, bytes32 commitHash) external {
        // First commit starts the auction
        if (_window[orderId].startedAt == 0) _startWindow(orderId);

        if (_phase(orderId) != PHASE_COMMIT) revert CommitPhaseEnded();
        uint256 epoch = _auctionEpoch[orderId];
        if (_commits[orderId][epoch][msg.sender] != bytes32(0)) revert AlreadyCommitted();

        _commits[orderId][epoch][msg.sender] = commitHash;

        emit Committed(orderId, msg.sender);
    }

    /// @inheritdoc IAuction
    function reveal(bytes32 orderId, uint256 outputAmount, bytes32 salt, bytes calldata originData) external {
        if (_phase(orderId) != PHASE_REVEAL) revert RevealPhaseNotActive();
        if (outputAmount == 0) revert ZeroOutputAmount();

        OrderValidator.decodeAndCheck(originData, orderId, outputAmount);

        uint256 epoch = _auctionEpoch[orderId];
        bytes32 commitHash = _commits[orderId][epoch][msg.sender];
        if (commitHash == bytes32(0)) revert NotCommitted();

        // TODO fix note[asm-keccak256]: use of inefficient hashing mechanism; consider using inline assembly
        // The committer is part of the commitment. Without it, a commit hash is public calldata.
        // Anyone can store it under their own address and open it with the values that the
        // original solver reveals. The copy duplicates the top quote and can win the fill.
        bytes32 expected = keccak256(abi.encode(orderId, msg.sender, outputAmount, salt));
        if (commitHash != expected) revert InvalidReveal();

        // Clear commit (prevent double reveal + gas refund)
        _commits[orderId][epoch][msg.sender] = bytes32(0);

        // Bound against the uint128 quote storage: an unchecked downcast would silently truncate
        // outputAmount > type(uint128).max, letting a sub-floor value enter Vickrey ranking.
        if (outputAmount > type(uint128).max) revert OutputAmountTooLarge();
        Quote memory quote = Quote({solver: msg.sender, outputAmount: uint128(outputAmount)});

        // A full quote list keeps the highest bids: the new quote replaces the lowest one, so the
        // reveal order cannot exclude a higher bid.
        Quote[] storage quotes = _quotes[orderId];
        if (quotes.length < maxQuotesPerOrder) {
            quotes.push(quote);
        } else {
            uint256 lowest;
            for (uint256 i = 1; i < quotes.length; i++) {
                if (quotes[i].outputAmount < quotes[lowest].outputAmount) lowest = i;
            }
            if (quote.outputAmount <= quotes[lowest].outputAmount) revert MaxQuotesReached();
            quotes[lowest] = quote;
        }

        emit QuoteSubmitted(orderId, msg.sender, outputAmount);
    }

    // ============ Router Functions ============

    /// @inheritdoc IAuction
    function resetAuction(bytes32 orderId, address winner) external onlyRouter {
        delete _quotes[orderId];
        _auctionEpoch[orderId]++;
        _startWindow(orderId);

        emit AuctionRestarted(orderId, winner);
    }

    // ============ Admin Functions ============

    /// @inheritdoc IAuction
    function setCommitPeriod(uint256 newPeriod) external onlyOwner {
        if (newPeriod == 0 || newPeriod > type(uint64).max) revert InvalidPeriod();
        uint256 oldPeriod = commitPeriod;
        commitPeriod = newPeriod;
        emit CommitPeriodUpdated(oldPeriod, newPeriod);
    }

    /// @inheritdoc IAuction
    function setRevealPeriod(uint256 newPeriod) external onlyOwner {
        if (newPeriod == 0 || newPeriod > type(uint64).max) revert InvalidPeriod();
        uint256 oldPeriod = revealPeriod;
        revealPeriod = newPeriod;
        emit RevealPeriodUpdated(oldPeriod, newPeriod);
    }

    /// @inheritdoc IAuction
    function setMaxQuotesPerOrder(uint256 newMax) external onlyOwner {
        if (newMax == 0 || newMax > MAX_QUOTES_PER_ORDER) revert InvalidMaxQuotes();
        uint256 oldMax = maxQuotesPerOrder;
        maxQuotesPerOrder = newMax;
        emit MaxQuotesUpdated(oldMax, newMax);
    }

    /// @inheritdoc IAuction
    function setRouter(address newRouter) external onlyOwner {
        if (newRouter == address(0)) revert ZeroRouter();
        address oldRouter = router;
        router = newRouter;
        emit RouterUpdated(oldRouter, newRouter);
    }

    /// @notice Disabled: without an owner the router and the periods can never change again
    function renounceOwnership() public view override onlyOwner {
        revert RenounceDisabled();
    }

    // ============ View Functions ============

    /// @inheritdoc IAuction
    function getWinner(bytes32 orderId) public view returns (address solver, uint256 amount) {
        Quote[] storage quotes = _quotes[orderId];
        if (quotes.length == 0) revert NoQuotes();
        if (_phase(orderId) < PHASE_ENDED) revert RevealNotEnded();

        uint128 bestAmount;
        uint128 secondAmount;
        address bestSolver;

        for (uint256 i = 0; i < quotes.length; i++) {
            if (quotes[i].outputAmount > bestAmount) {
                secondAmount = bestAmount;
                bestAmount = quotes[i].outputAmount;
                bestSolver = quotes[i].solver;
            } else if (quotes[i].outputAmount > secondAmount) {
                secondAmount = quotes[i].outputAmount;
            }
        }

        return (bestSolver, secondAmount > 0 ? secondAmount : bestAmount);
    }

    /// @inheritdoc IAuction
    function isAuctionEnded(bytes32 orderId) external view returns (bool) {
        return _phase(orderId) == PHASE_ENDED;
    }

    /// @inheritdoc IAuction
    function getQuotes(bytes32 orderId) external view returns (Quote[] memory) {
        return _quotes[orderId];
    }

    /// @inheritdoc IAuction
    function getQuoteCount(bytes32 orderId) external view returns (uint256) {
        return _quotes[orderId].length;
    }

    /// @inheritdoc IAuction
    function getCommitDeadline(bytes32 orderId) external view returns (uint256) {
        Window memory w = _window[orderId];
        if (w.startedAt == 0) return 0;
        return uint256(w.startedAt) + w.commitPeriod;
    }

    /// @inheritdoc IAuction
    function getRevealDeadline(bytes32 orderId) external view returns (uint256) {
        Window memory w = _window[orderId];
        if (w.startedAt == 0) return 0;
        return uint256(w.startedAt) + w.commitPeriod + w.revealPeriod;
    }

    /// @inheritdoc IAuction
    function hasSolverCommitted(bytes32 orderId, address solver) external view returns (bool) {
        return _commits[orderId][_auctionEpoch[orderId]][solver] != bytes32(0);
    }

    /// @inheritdoc IAuction
    function auctionStartedAt(bytes32 orderId) external view returns (uint256) {
        return _window[orderId].startedAt;
    }

    // ============ Internal ============

    /// @notice Starts the auction window of an order with the current periods
    function _startWindow(bytes32 orderId) internal {
        _window[orderId] = Window({
            startedAt: uint64(block.timestamp), commitPeriod: uint64(commitPeriod), revealPeriod: uint64(revealPeriod)
        });
    }

    /// @notice Returns the current auction phase for an order
    function _phase(bytes32 orderId) internal view returns (uint8) {
        Window memory w = _window[orderId];
        if (w.startedAt == 0) return PHASE_NONE;
        uint256 commitEnd = uint256(w.startedAt) + w.commitPeriod;
        if (block.timestamp < commitEnd) return PHASE_COMMIT;
        if (block.timestamp < commitEnd + w.revealPeriod) return PHASE_REVEAL;
        return PHASE_ENDED;
    }
}
