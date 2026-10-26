// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

/// @notice Interface for solver collateral: check, lock, unlock, slash, view
interface ISolverEscrow {
    struct BalanceInfo {
        address token;
        uint256 total;
        uint256 locked;
        uint256 available;
    }

    // ============ Events ============

    event Deposited(address indexed solver, address indexed token, uint256 amount);
    event Withdrawn(address indexed solver, address indexed token, uint256 amount);
    event AuthorizedCallerSet(address indexed caller);
    event CollateralLocked(bytes32 indexed orderId, address indexed solver, address token, uint256 amount);
    event CollateralUnlocked(bytes32 indexed orderId);
    event CollateralSlashed(bytes32 indexed orderId, address indexed solver, address token, uint256 amount);
    event CollateralBpsUpdated(uint256 oldBps, uint256 newBps);
    event RewardDistributed(address indexed receiver, address indexed token, uint256 amount);
    /// @notice The slashed pool of `token` holds less than the reward, so no reward was paid
    event RewardSkipped(address indexed token, uint256 reward, uint256 available);

    // ============ Errors ============

    /// @notice Thrown when deposit or withdrawal amount is zero
    error InvalidAmount();
    /// @notice Thrown when caller is not the authorized caller (Router)
    error UnauthorizedCaller();
    /// @notice Thrown when collateral bps is zero or exceeds 10000
    error InvalidBps();
    /// @notice Thrown when solver has not approved escrow as ERC6909 operator
    error OperatorNotApproved();
    /// @notice Thrown when the solver balance after a withdrawal is not the expected one
    error WithdrawalFailed();
    /// @notice Thrown when authorized caller has already been set
    error AuthorizedCallerAlreadySet();
    /// @notice Thrown when a zero address is supplied where one is not allowed
    error ZeroAddress();
    /// @notice Thrown when available balance is insufficient
    error InsufficientAvailableBalance();
    /// @notice Thrown when a collateral lock already exists for the order
    error LockAlreadyExists();
    /// @notice Thrown when no collateral lock exists for the order
    error LockNotFound();

    // ============ Functions ============

    /// @notice Distribute reward from slashed pool to receiver (REWARD_BPS of orderAmountIn)
    /// @param token The underlying token
    /// @param orderAmountIn The order's input amount (reward calculated as percentage)
    /// @param receiver The address to receive underlying tokens
    /// @return reward The amount distributed (0 if insufficient slashed balance)
    function distributeReward(address token, uint256 orderAmountIn, address receiver) external returns (uint256 reward);

    /// @notice Returns true if solver has sufficient available (unlocked) collateral
    function hasMinCollateral(address solver, address token, uint256 outputAmount) external view returns (bool);

    /// @notice Lock solver collateral for a claimed order, moving it into escrow custody
    function lockCollateral(bytes32 orderId, address solver, address token, uint256 amount) external;

    /// @notice Unlock solver collateral after successful fill, returning custody to the solver
    function unlockCollateral(bytes32 orderId) external;

    /// @notice Slash locked collateral into the escrow's slashed pool
    function slashCollateral(bytes32 orderId) external;

    /// @notice Collateral basis points (e.g. 1000 = 10%)
    function collateralBps() external view returns (uint256);

    /// @notice Calculate collateral amount for a given output amount
    function getCollateralAmount(uint256 outputAmount) external view returns (uint256);

    /// @notice Get ERC6909 balance for an owner and token (total, locked, available)
    function getBalance(address owner, address token)
        external
        view
        returns (uint256 total, uint256 locked, uint256 available);

    /// @notice Get ERC6909 balances for an owner across multiple tokens
    function getBalances(address owner, address[] calldata tokens) external view returns (BalanceInfo[] memory);
}
