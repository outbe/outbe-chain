// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ICredis {
    event PositionCreated(uint256 indexed positionId, address indexed smartAccount, uint256 anadosisAmount);

    event AnadosisPaid(uint256 indexed positionId, uint32 anadosisNumber, uint256 anadosisAmount);

    /// Emitted when an expired position's outstanding pledged collateral is burned.
    event CollateralBurned(uint256 indexed positionId, address indexed smartAccount, uint256 gratisBurned);

    struct Position {
        uint256 positionId;
        address asset;
        address smartAccount;
        uint256 totalAnadosisAmount;
        uint256 outstandingAnadosisAmount;
        uint256 totalGratisAmount;
        uint256 outstandingGratisAmount;
        uint32 nextAnadosisNumber;
        uint64 createdAt;
        uint256 credisPrincipal;
        uint256 entryPriceMinor;
        uint256 currencyRate;
        uint16 issuanceCurrency;
        // Pledger EOA ciphertext (not an address). The enclave recovers
        // the plaintext EOA on-chain via a RevealOwner round-trip.
        bytes eoaCiphertext;
    }

    struct Anadosis {
        uint32 anadosisNumber;
        uint64 dueDate;
        uint64 paidAt;
        uint256 anadosisAmount;
        uint256 gratisAmount;
    }

    function getPosition(uint256 positionId) external view returns (Position memory);

    function getPositionsByAddress(address smartAccount) external view returns (Position[] memory);

    function getAllPositions() external view returns (Position[] memory);

    function hasOverdueAnadosis(address smartAccount) external view returns (bool);

    function getNextAnadosis(uint256 positionId) external view returns (Anadosis memory);

    function getPositionAnadosis(uint256 positionId) external view returns (Anadosis[] memory);

    function credisOf(address smartAccount) external view returns (uint256);

    function outstandingAnadosisOf(address smartAccount) external view returns (uint256);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
