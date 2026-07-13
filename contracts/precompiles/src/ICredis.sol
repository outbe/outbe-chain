// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredis {
    event PositionCreated(uint256 indexed positionId, address indexed bundleAccount, uint256 anadosisAmount);

    event AnadosisPaid(uint256 indexed positionId, uint32 anadosisNumber, uint256 anadosisAmount);

    struct Position {
        uint256 positionId;
        address asset;
        address bundleAccount;
        uint256 totalAnadosisAmount;
        uint256 outstandingAnadosisAmount;
        uint256 totalGratisAmount;
        uint256 outstandingGratisAmount;
        uint32 nextAnadosisNumber;
        uint64 createdAt;
        uint256 credisPrincipal;
        uint256 refinancingRate;
        uint16 issuanceCurrency;
    }

    struct Anadosis {
        uint32 anadosisNumber;
        uint64 dueDate;
        uint64 paidAt;
        uint256 anadosisAmount;
        uint256 gratisAmount;
    }

    function getPosition(uint256 positionId) external view returns (Position memory);

    function getPositionsByAddress(address bundleAccount) external view returns (Position[] memory);

    function getAllPositions() external view returns (Position[] memory);

    function hasOverdueAnadosis(address bundleAccount) external view returns (bool);

    function getNextAnadosis(uint256 positionId) external view returns (Anadosis memory);

    function getPositionAnadosis(uint256 positionId) external view returns (Anadosis[] memory);

    function credisOf(address bundleAccount) external view returns (uint256);

    function outstandingAnadosisOf(address bundleAccount) external view returns (uint256);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
