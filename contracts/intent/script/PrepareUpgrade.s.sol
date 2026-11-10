// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {SALT_VERSION} from "@shared/SaltVersion.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {Auction} from "../src/Auction.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";
import {RouterAllocator} from "../src/allocators/RouterAllocator.sol";
import {SolverAllocator} from "../src/allocators/SolverAllocator.sol";
import {Router} from "../src/router/Router.sol";
import {IntentCreate3} from "./IntentCreate3.sol";

/// @dev Prepares an upgrade of the five intent proxies on the chains of the stack (see
///      UpgradeProposal). Each implementation gets the immutables of the live proxy: The Compact,
///      the lock tag, the hub, the escrow and the auction.
///
/// Env: the UpgradeProposal ones, `CREATE3_FACTORY_ADDRESS` and `CONTRACT_SALT` (default `SALT_VERSION`).
contract PrepareUpgrade is UpgradeProposal {
    function _proxies(address deployer) internal view override returns (address[] memory proxies) {
        string[5] memory labels = [
            IntentCreate3.AUCTION,
            IntentCreate3.SOLVER_ESCROW,
            IntentCreate3.SOLVER_ALLOCATOR,
            IntentCreate3.ROUTER_ALLOCATOR,
            IntentCreate3.ROUTER
        ];
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        proxies = new address[](labels.length);
        for (uint256 i = 0; i < labels.length; i++) {
            proxies[i] = IntentCreate3.predict(factory, deployer, labels[i], salt);
        }
    }

    function _newImplementation(uint256 index, address proxy) internal override returns (address) {
        if (index == 0) return address(new Auction());
        if (index == 1) {
            SolverEscrow escrow = SolverEscrow(proxy);
            return address(new SolverEscrow(address(escrow.COMPACT()), escrow.LOCK_TAG()));
        }
        if (index == 2) return address(new SolverAllocator(address(SolverAllocator(proxy).COMPACT())));
        if (index == 3) return address(new RouterAllocator(address(RouterAllocator(proxy).COMPACT())));
        Router router = Router(proxy);
        return address(
            new Router(
                address(router.bridge()),
                address(router.COMPACT()),
                router.LOCK_TAG(),
                address(router.SOLVER_ESCROW()),
                address(router.AUCTION())
            )
        );
    }

    function _deployerOwns(address proxy, address deployer) internal view override returns (bool) {
        return Ownable(proxy).owner() == deployer;
    }
}
