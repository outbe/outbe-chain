// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITribute {
    error TributeNotFound();
    error TributeAlreadyExists();
    error InvalidOwner();
    error IssuanceAmountMustBePositive();
    error WorldwideDayIsSealed();
    error OwnerBalanceOverflow();
    error QueryLimitExceeded();
    error DayResponseScopeMismatch();
    error CalculationView(string value);
    error WorldwideDayNotCompletedAndEmpty();
    error PrivateAmountReadFailed(string value);
    error CanonicalIdentityMismatch(uint256 expected, uint256 found);
    error InvalidLeague(uint16 value);
    error MetadosisNotCompleted();
    error NodIssueBeforeCompletion();
    error CallerNotTributeOwner();
    error TributeStateChangedAfterLoading();
    error LysisArithmetic(string reason);
    error DateStep(uint8 kind, uint32 dateKey);
    error TributeForfeited();
    error TributeNotForfeited();

    event TributeOffered(
        uint256 indexed tributeId,
        address indexed owner,
        uint32 indexed worldwideDay,
        uint16 issuanceCurrency,
        uint16 referenceCurrency
    );

    event WorldwideDaySealed(uint32 indexed worldwideDay);

    event TributeBurned(uint256 indexed tributeId, address indexed owner);

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
    function issueNod(uint256 tributeId) external returns (uint256 nodId);
    function clean(uint256 tributeId) external;
    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
