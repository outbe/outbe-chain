// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITribute {
    error TributeNotFound();
    error TributeAlreadyExists();
    error InvalidOwner();
    error SettlementAmountMustBePositive();
    error WorldwideDaySealed();
    error OwnerBalanceOverflow();
    error QueryLimitExceeded();
    error DayResponseScopeMismatch();
    error CalculationView(string value);
    error WorldwideDayNotCompletedAndEmpty();
    error PrivateAmountReadFailed(string value);
    error CanonicalIdentityMismatch(uint256 expected, uint256 found);
    error InvalidLeague(uint16 value);
    error MetadosisNotCompleted();
    error NodCreationBeforeCompletion();
    error CallerNotTributeOwner();
    error EntryPriceUnavailable(uint32 priceDay, uint16 currency);
    error TributeStateChangedAfterLoading();
    error LysisArithmetic(string reason);
    error DateStep(uint8 kind, uint32 dateKey);
    error TributeForfeited();
    error TributeNotForfeited();

    event TributeIssued(
        address indexed owner,
        uint256 tributeId,
        uint32 worldwideDay,
        bytes issuanceAmountMinor,
        uint16 settlementCurrency,
        bytes nominalAmountMinor
    );

    event TributeBurned(uint256 tributeId, address owner, uint32 worldwideDay);

    event TributeWorldwideDaySealed(uint32 indexed worldwideDay, bool isSealed);

    /// @notice Canonical projection event for certified Lysis retirement and
    /// Metadosis terminal authorities.
    event TributePartitionRetired(uint32 indexed worldwideDay);

    event CertifiedTributePartitionRetired(
        bytes32 indexed activationCallId,
        uint32 indexed worldwideDay,
        uint64 sourceGeneration,
        uint32 consumedCount,
        bytes consumedNominalTotalMinor,
        uint64 retiredGeneration,
        bytes32 stateEventDigest
    );

    function name() external view returns (string memory);
    function symbol() external view returns (string memory);
    function balanceOf(address owner) external view returns (uint256);
    function ownerOf(uint256 tributeId) external view returns (address);
    function tokenURI(uint256 tributeId) external view returns (string memory);
    function getDayTotals(uint32 worldwideDay)
        external
        view
        returns (uint32 tributeCount, bytes memory tributeNominalTotalMinor, bool isSealed);
    function tokensByOwner(address owner) external view returns (uint256[] memory tokenIds);
    function getLeague(uint256 tributeId) external view returns (uint16 league);
    function getDayLeagueFraction(uint32 worldwideDay, uint16 league) external view returns (uint64 fraction);
    function createNod(uint256 tributeId) external returns (uint256 nodId);
    function clean(uint256 tributeId) external;
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
