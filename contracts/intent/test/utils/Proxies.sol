// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

import {Auction} from "../../src/Auction.sol";
import {SolverEscrow} from "../../src/SolverEscrow.sol";
import {SolverAllocator} from "../../src/allocators/SolverAllocator.sol";

/// @dev Deploys each contract behind an ERC1967 proxy, as the deploy scripts do.
library Proxies {
    function proxy(address implementation, bytes memory initData) internal returns (address) {
        return address(new ERC1967Proxy(implementation, initData));
    }

    function auction(address owner) internal returns (Auction) {
        return Auction(proxy(address(new Auction()), abi.encodeCall(Auction.initialize, (owner))));
    }

    function solverAllocator(address compact, address owner) internal returns (SolverAllocator) {
        return SolverAllocator(
            proxy(address(new SolverAllocator(compact)), abi.encodeCall(SolverAllocator.initialize, (owner)))
        );
    }

    function solverEscrow(address compact, bytes12 lockTag, uint256 collateralBps, address owner)
        internal
        returns (SolverEscrow)
    {
        return SolverEscrow(
            proxy(
                address(new SolverEscrow(compact, lockTag)),
                abi.encodeCall(SolverEscrow.initialize, (owner, collateralBps))
            )
        );
    }
}
