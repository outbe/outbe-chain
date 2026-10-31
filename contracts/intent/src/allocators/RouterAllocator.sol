// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

import {IAllocator} from "the-compact/src/interfaces/IAllocator.sol";
import {ITheCompact} from "the-compact/src/interfaces/ITheCompact.sol";
import {IdLib} from "the-compact/src/lib/IdLib.sol";
import {Scope} from "the-compact/src/types/Scope.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";

/// @title RouterAllocator
/// @notice Allocator for The Compact that authorizes claims from registered router operators.
/// @dev Pure validation contract - no token operations. Registers itself with The Compact on deploy.
///      The owner adds authorized operators (LayerZeroRouter, HyperlaneRouter, etc.) post-deploy
///      via addOperator().
///      UUPS upgradeable behind an ERC1967 proxy. The proxy registers itself with The Compact in
///      `initialize`, so the allocator id stays with the proxy address across upgrades.
contract RouterAllocator is IAllocator, OwnableUpgradeable, UUPSUpgradeable {
    // ============ Constants ============

    string public constant VERSION = "1.0.0";

    // ============ Immutables ============

    /// @notice The Compact contract
    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    ITheCompact public immutable COMPACT;

    // ============ State ============

    /// @custom:storage-location erc7201:outbe.intent.RouterAllocator
    struct RouterAllocatorStorage {
        /// @notice Allocator ID assigned by The Compact on registration
        uint96 allocatorId;
        /// @notice Authorized router operators (one per chain per messaging layer)
        /// @dev operator = msg.sender of allocatedTransfer() on The Compact = router address
        mapping(address => bool) authorizedOperators;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intent.RouterAllocator")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x4b5af51c009f107f822b9d40b713bb7b7f11d4444c2f9179e6f08e2564d36700;

    // ============ Events ============

    event OperatorAdded(address indexed operator);
    event OperatorRemoved(address indexed operator);

    // ============ Errors ============

    /// @notice Thrown when the operator is not registered for this allocator
    error UnauthorizedOperator(address operator);
    /// @notice Thrown when a claim has expired
    error ClaimExpired(uint256 expires, uint256 currentTime);
    /// @notice Thrown when a zero address is supplied as an operator
    error ZeroOperator();

    // ============ Constructor ============

    /// @param _compact The Compact contract address
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address _compact) {
        COMPACT = ITheCompact(_compact);
        _disableInitializers();
    }

    function initialize(address _owner) external initializer {
        __Ownable_init(_owner);
        // The Compact accepts the registration because the proxy itself calls it.
        // Returns the unique allocatorId used to build lockTags.
        _s().allocatorId = COMPACT.__registerAllocator(address(this), "");
    }

    function _authorizeUpgrade(address) internal override onlyOwner {}

    function _s() private pure returns (RouterAllocatorStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @notice Allocator ID assigned by The Compact on registration
    function ALLOCATOR_ID() public view returns (uint96) {
        return _s().allocatorId;
    }

    /// @notice Authorized router operators (one per chain per messaging layer)
    function authorizedOperators(address operator) public view returns (bool) {
        return _s().authorizedOperators[operator];
    }

    // ============ Owner Functions ============

    /// @notice Authorize a router contract to trigger allocations
    /// @dev Call after deploying LayerZeroRouter (or any other router) - pass its address here
    function addOperator(address _operator) external onlyOwner {
        if (_operator == address(0)) revert ZeroOperator();
        _s().authorizedOperators[_operator] = true;
        emit OperatorAdded(_operator);
    }

    /// @notice Revoke operator authorization
    function removeOperator(address _operator) external onlyOwner {
        _s().authorizedOperators[_operator] = false;
        emit OperatorRemoved(_operator);
    }

    // ============ View ============

    /// @notice Build a lockTag for the given scope and reset period using this allocator's ID.
    /// @dev Call to get LOCK_TAG before configuring a router via setCompactConfig().
    ///      Example: buildLockTag(Scope.Multichain, ResetPeriod.OneDay)
    function buildLockTag(Scope scope, ResetPeriod resetPeriod) external view returns (bytes12) {
        return IdLib.toLockTag(_s().allocatorId, scope, resetPeriod);
    }

    function getId(bytes12 lockTag, address token) public pure returns (uint256) {
        return (uint256(uint96(bytes12(lockTag))) << 160) | uint160(token);
    }

    // ============ IAllocator ============

    /// @inheritdoc IAllocator
    /// @dev The Compact calls this before every allocatedTransfer().
    ///      operator = the address that called allocatedTransfer() = our router.
    function attest(
        address operator,
        address, /* from */
        address, /* to */
        uint256, /* id */
        uint256 /* amount */
    )
        external
        view
        override
        returns (bytes4)
    {
        if (!_s().authorizedOperators[operator]) revert UnauthorizedOperator(operator);
        return IAllocator.attest.selector;
    }

    /// @inheritdoc IAllocator
    /// @dev The Compact calls this when it processes a claim.
    ///      arbiter = the address that called claim() = our router.
    function authorizeClaim(
        bytes32, /* claimHash */
        address arbiter,
        address, /* sponsor */
        uint256, /* nonce */
        uint256 expires,
        uint256[2][] calldata, /* idsAndAmounts */
        bytes calldata /* allocatorData */
    ) external view override returns (bytes4) {
        if (block.timestamp > expires) revert ClaimExpired(expires, block.timestamp);
        if (!_s().authorizedOperators[arbiter]) revert UnauthorizedOperator(arbiter);
        return IAllocator.authorizeClaim.selector;
    }

    /// @inheritdoc IAllocator
    /// @dev Off-chain check before submitting a claim.
    function isClaimAuthorized(
        bytes32, /* claimHash */
        address arbiter,
        address, /* sponsor */
        uint256, /* nonce */
        uint256 expires,
        uint256[2][] calldata, /* idsAndAmounts */
        bytes calldata /* allocatorData */
    ) external view override returns (bool) {
        return _s().authorizedOperators[arbiter] && block.timestamp <= expires;
    }
}
