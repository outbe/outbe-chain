// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";

import {IAllocator} from "the-compact/src/interfaces/IAllocator.sol";
import {ITheCompact} from "the-compact/src/interfaces/ITheCompact.sol";
import {IdLib} from "the-compact/src/lib/IdLib.sol";
import {ResetPeriod} from "the-compact/src/types/ResetPeriod.sol";
import {Scope} from "the-compact/src/types/Scope.sol";

/// @title SolverAllocator
/// @notice Allocator for The Compact that manages solver collateral resource locks.
/// @dev Roles:
///      - Standard ERC6909 transfers (attest): blocked - solvers must use escrow.withdraw().
///      - Allocated transfers / claims (authorizeClaim): only arbiter (escrow) is authorized.
///
/// Deploy order:
///   1. Deploy SolverAllocator(_compact) behind a proxy, initialize(owner)
///   2. lockTag = allocator.buildLockTag(Scope.ChainSpecific, ResetPeriod.ThirtyDays)
///   3. Deploy SolverEscrow(_compact, lockTag)
///   4. allocator.setArbiter(address(escrow))
///
/// UUPS upgradeable behind an ERC1967 proxy. The proxy registers itself with The Compact in
/// `initialize`, so the allocator id stays with the proxy address across upgrades.
contract SolverAllocator is IAllocator, OwnableUpgradeable, UUPSUpgradeable {
    // ============ Constants ============

    string public constant VERSION = "1.0.0";

    // ============ Immutables ============

    /// @notice The Compact contract
    /// @custom:oz-upgrades-unsafe-allow state-variable-immutable
    ITheCompact public immutable COMPACT;

    // ============ State ============

    /// @custom:storage-location erc7201:outbe.intent.SolverAllocator
    struct SolverAllocatorStorage {
        /// @notice Allocator ID assigned by The Compact on registration
        uint96 allocatorId;
        /// @notice Authorized arbiter for withdrawals and slashing (SolverEscrow).
        ///         The owner sets it once after the escrow is deployed.
        address arbiter;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intent.SolverAllocator")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x6c10a7fd0f39fda798618c7bb9815bcc4909518d7bac8c9f27123a2f6b638600;

    // ============ Events ============

    event ArbiterSet(address indexed arbiter);

    // ============ Errors ============

    /// @notice Thrown when attest() is called (direct ERC6909 transfers are blocked)
    error DirectTransferBlocked();
    /// @notice Thrown when caller is not the authorized arbiter
    error UnauthorizedArbiter();
    /// @notice Thrown when caller is not the contract owner
    error UnauthorizedOwner();
    /// @notice Thrown when arbiter has already been set
    error ArbiterAlreadySet();
    /// @notice Thrown when a zero address is supplied as the arbiter
    error ZeroArbiter();
    /// @notice Thrown when a claim has expired
    error ClaimExpired(uint256 expires, uint256 currentTime);

    // ============ Constructor ============

    /// @param _compact The Compact contract address
    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address _compact) {
        COMPACT = ITheCompact(_compact);
        _disableInitializers();
    }

    /// @param _owner Can call setArbiter() once and authorizes upgrades
    function initialize(address _owner) external initializer {
        __Ownable_init(_owner);
        _s().allocatorId = COMPACT.__registerAllocator(address(this), "");
    }

    function _authorizeUpgrade(address) internal override onlyOwner {}

    function _s() private pure returns (SolverAllocatorStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @notice Allocator ID assigned by The Compact on registration
    function ALLOCATOR_ID() public view returns (uint96) {
        return _s().allocatorId;
    }

    /// @notice Authorized arbiter for withdrawals and slashing (SolverEscrow)
    function arbiter() public view returns (address) {
        return _s().arbiter;
    }

    // ============ Admin ============

    /// @notice Set the arbiter contract address. Only the owner can call this, and only once.
    /// @param _arbiter SolverEscrow contract address
    function setArbiter(address _arbiter) external {
        if (msg.sender != owner()) revert UnauthorizedOwner();
        if (_arbiter == address(0)) revert ZeroArbiter();
        if (_s().arbiter != address(0)) revert ArbiterAlreadySet();
        _s().arbiter = _arbiter;
        emit ArbiterSet(_arbiter);
    }

    // ============ View ============

    /// @notice Build a lockTag using this allocator's ID.
    function buildLockTag(Scope scope, ResetPeriod resetPeriod) external view returns (bytes12) {
        return IdLib.toLockTag(_s().allocatorId, scope, resetPeriod);
    }

    // ============ IAllocator ============

    /// @inheritdoc IAllocator
    /// @dev Allows ERC6909 transfers only when the operator is the arbiter (escrow).
    ///      This function blocks all other direct transfers. Solvers must use escrow.withdraw().
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
        if (operator != _s().arbiter) revert DirectTransferBlocked();
        return IAllocator.attest.selector;
    }

    /// @inheritdoc IAllocator
    /// @dev The Compact calls this when it processes allocatedTransfer() and claim().
    function authorizeClaim(
        bytes32 claimHash,
        address claimArbiter,
        address sponsor,
        uint256 nonce,
        uint256 expires,
        uint256[2][] calldata idsAndAmounts,
        bytes calldata allocatorData
    ) external view override returns (bytes4) {
        if (block.timestamp > expires) revert ClaimExpired(expires, block.timestamp);
        if (!isClaimAuthorized(claimHash, claimArbiter, sponsor, nonce, expires, idsAndAmounts, allocatorData)) {
            revert UnauthorizedArbiter();
        }
        return IAllocator.authorizeClaim.selector;
    }

    /// @inheritdoc IAllocator
    function isClaimAuthorized(
        bytes32, /* claimHash */
        address claimArbiter,
        address, /* sponsor */
        uint256, /* nonce */
        uint256 expires,
        uint256[2][] calldata, /* idsAndAmounts */
        bytes calldata /* allocatorData */
    ) public view override returns (bool) {
        // Only the arbiter (escrow) can claim, and not past expiry. Solvers must go through escrow.
        return claimArbiter == _s().arbiter && block.timestamp <= expires;
    }
}
