// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ICca — Credis Card Agent registry.
/// @notice A CCA originates Credis positions on behalf of card owners. It is paid
///         from the daily CCA emission pool for originations it brings in, and
///         penalized when a position it originated is voided. Registration is
///         permissionless.
interface ICca {
    /// @notice Registration state. `Suspended` is reserved for a lapsed bond
    ///         top-up grace; nothing sets it yet.
    enum State {
        Active,
        Suspended,
        Deregistered
    }

    struct Cca {
        address cca;
        /// Performance multiplier, 1e18 scaled. Starts at 1e18 and never exceeds it.
        uint256 multiplier;
        uint64 registeredAt;
        State state;
    }

    event CcaRegistered(address indexed cca, uint256 multiplier);
    event CcaDeregistered(address indexed cca);
    /// @notice Emitted when a void cuts the agent's multiplier.
    /// @param unpaidShare Share of the position's principal left unpaid, 1e18 scaled.
    event MultiplierPenalized(address indexed cca, uint256 multiplier, uint256 unpaidShare);
    /// @notice Emitted when settled principal restores part of the multiplier.
    event MultiplierRecovered(address indexed cca, uint256 multiplier);
    /// @notice Emitted for an origination that earned reward units. Originations
    ///         that earn nothing (below the dust guard, or a second position for
    ///         the same owner on the same day) emit nothing.
    event OriginationRecorded(
        address indexed cca, address indexed smartAccount, uint32 indexed worldwideDay, uint256 units
    );

    /// @notice Register `msg.sender` as a CCA, starting at an unpenalized multiplier.
    function register() external;

    /// @notice Retire `msg.sender`. Its open positions are unaffected and still
    ///         penalize the record if they void.
    function deregister() external;

    function getCca(address cca) external view returns (Cca memory);

    /// @notice Whether `cca` may open new positions: registered, active, and not
    ///         in quarantine (multiplier at or above 0.5). False rather than a
    ///         revert for an address that never registered.
    function canOriginate(address cca) external view returns (bool);

    /// @notice The agent's multiplier, 1e18 scaled. Returns 1e18 for an address
    ///         that never registered.
    function multiplierOf(address cca) external view returns (uint256);

    /// @notice Raw origination units earned on a worldwide day, 1e18 scaled and
    ///         before the multiplier is applied.
    function originationUnits(uint32 worldwideDay, address cca) external view returns (uint256);

    function supportsInterface(bytes4 interfaceId) external view returns (bool);
}
