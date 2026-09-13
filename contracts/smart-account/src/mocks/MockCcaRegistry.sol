// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

import {ICca} from "@precompiles/ICca.sol";

/// @notice Standing-only test double, etched at the protocol address.
/// @dev Empty storage means Unknown. Tests fund and bond their CCA explicitly.
contract MockCcaRegistry {
    mapping(address => ICca.State) private _states;
    mapping(address => uint256) private _bonds;

    function bond() external payable {
        require(msg.value > 0, "positive bond required");
        _bonds[msg.sender] += msg.value;
        _states[msg.sender] = _bonds[msg.sender] >= 1_000_000_000 ether ? ICca.State.Active : ICca.State.Suspended;
    }

    function setState(address cca, ICca.State state) external {
        _states[cca] = state;
    }

    function getCcaState(address cca) external view returns (ICca.State) {
        return _states[cca];
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == 0x01ffc9a7;
    }
}
