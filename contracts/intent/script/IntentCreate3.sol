// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Create3Factory} from "@shared/Create3Factory.sol";

/// @dev CREATE3 labels of the intent contracts. Each proxy address depends only on (factory,
///      deployer, label, salt), so every chain gets the same address for each contract.
library IntentCreate3 {
    string internal constant ROUTER = "Router";
    string internal constant ROUTER_ALLOCATOR = "RouterAllocator";
    string internal constant AUCTION = "Auction";
    string internal constant SOLVER_ESCROW = "SolverEscrow";
    string internal constant SOLVER_ALLOCATOR = "SolverAllocator";

    /// @dev The factory namespaces this with the caller, so the salt omits the deployer.
    function salt(string memory label, string memory version) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(label, version));
    }

    function predict(address factory, address deployer, string memory label, string memory version)
        internal
        view
        returns (address)
    {
        return Create3Factory(factory).predict(deployer, salt(label, version));
    }
}
