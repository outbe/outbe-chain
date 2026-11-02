// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {CROSSCHAIN_GOV, ICrosschainGov} from "@precompiles/ICrosschainGov.sol";

/// @dev Gives every intent contract on this chain to governance (OIP-00369): the Router, the
///      Auction, the SolverEscrow and the two allocators. Governance is CrosschainGov on Outbe and
///      its Interchain Account on any other chain. Each contract is a plain Ownable, so
///      `transferOwnership` moves the owner at once. The script reads every owner back.
///      Run it last: after DeployAll and ConfigureRouter, which need the deployer as the owner.
///      After it, an upgrade or a configuration change is a governance proposal (`upgradeContract`).
///
/// Required env: `DEPLOYER_PK` (the current owner), `OUTBE_CHAIN_ID`, `ROUTER_ADDRESS`,
///   `AUCTION_ADDRESS`, `ESCROW_ADDRESS`, `ALLOCATOR_ADDRESS` (RouterAllocator),
///   `SOLVER_ALLOCATOR_ADDRESS`, and on another chain `OUTBE_RPC_URL`.
contract HandOver is Script {
    error DomainTooLarge(uint256 chainId);
    error NotHandedOver(address target, address owner);

    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        address[5] memory owned = [
            vm.envAddress("ROUTER_ADDRESS"),
            vm.envAddress("AUCTION_ADDRESS"),
            vm.envAddress("ESCROW_ADDRESS"),
            vm.envAddress("ALLOCATOR_ADDRESS"),
            vm.envAddress("SOLVER_ALLOCATOR_ADDRESS")
        ];
        // Read before the broadcast: on a chain other than Outbe this asks the Outbe node.
        address governance = governanceOwner();

        vm.startBroadcast(deployerPk);
        handOver(governance, owned);
        vm.stopBroadcast();
        console2.log("handed over to governance:", governance);
    }

    /// @dev Governance on this chain: CrosschainGov on Outbe, its Interchain Account elsewhere.
    function governanceOwner() public returns (address) {
        if (block.chainid == vm.envUint("OUTBE_CHAIN_ID")) return CROSSCHAIN_GOV;
        if (block.chainid > type(uint32).max) revert DomainTooLarge(block.chainid);
        return _governanceAccount(uint32(block.chainid));
    }

    /// @dev Requires an active broadcast from the current owner. A contract that governance
    ///      already owns is skipped.
    function handOver(address governance, address[5] memory owned) public {
        for (uint256 i = 0; i < owned.length; i++) {
            Ownable c = Ownable(owned[i]);
            if (c.owner() != governance) c.transferOwnership(governance);
            if (c.owner() != governance) revert NotHandedOver(owned[i], c.owner());
        }
    }

    /// @dev Asks CrosschainGov on Outbe for its Interchain Account on `domain`. A fork cannot run
    ///      the precompile, so this is a raw `eth_call` to the Outbe node. `virtual` so a test
    ///      harness can supply the account.
    function _governanceAccount(uint32 domain) internal virtual returns (address) {
        string memory params = string.concat(
            '[{"to":"',
            vm.toString(CROSSCHAIN_GOV),
            '","data":"',
            vm.toString(abi.encodeCall(ICrosschainGov.remoteAccount, (domain))),
            '"},"latest"]'
        );
        // forge-std 1.8.1 has no `rpc(url, method, params)` in its Vm interface, but forge has the
        // cheatcode, so the script calls it by signature.
        (bool ok, bytes memory ret) = address(vm)
            .call(
                abi.encodeWithSignature("rpc(string,string,string)", vm.envString("OUTBE_RPC_URL"), "eth_call", params)
            );
        require(ok, "eth_call to the Outbe node failed");
        return abi.decode(abi.decode(ret, (bytes)), (address));
    }
}
