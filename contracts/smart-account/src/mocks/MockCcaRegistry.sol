// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {ICcaRegistry} from "@precompiles/ICcaRegistry.sol";

/// @notice Standing-only test double, etched at the protocol address.
/// @dev Empty storage is unregistered. Tests fund and bond their CCA explicitly.
contract MockCcaRegistry {
    mapping(address => ICcaRegistry.State) private _states;
    mapping(address => uint256) private _bonds;

    function bond(string calldata name) external payable {
        require(msg.value > 0, "positive bond required");
        require(bytes(name).length > 0, "CCA name must be nonempty");
        _bonds[msg.sender] += msg.value;
        _states[msg.sender] =
            _bonds[msg.sender] >= 1_000_000_000 ether ? ICcaRegistry.State.Active : ICcaRegistry.State.Bonding;
    }

    function setState(address cca, ICcaRegistry.State state) external {
        _states[cca] = state;
    }

    function getCcaState(address cca) external view returns (ICcaRegistry.State) {
        require(_bonds[cca] != 0 || _states[cca] != ICcaRegistry.State.Bonding, "CCA is not registered");
        return _states[cca];
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == 0x01ffc9a7;
    }
}
