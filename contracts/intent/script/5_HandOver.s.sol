// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {CROSSCHAIN_GOV, ICrosschainGov} from "@precompiles/ICrosschainGov.sol";

/// @dev The `rpc(urlOrAlias, method, params)` cheatcode. The forge-std version of this project does not declare it.
interface IRpcCheatcode {
    function rpc(string calldata urlOrAlias, string calldata method, string calldata params)
        external
        returns (bytes memory data);
}

/// @dev The five intent contracts. A zero address means the contract is not deployed.
struct IntentContracts {
    address auction;
    address escrow;
    address solverAllocator;
    address router;
    address allocator;
}

/// @dev Gives every intent contract to governance (OIP-00369), as `contracts/crosschain/script/4_HandOver.s.sol`
///      does for the hub. The deployer owns the contracts while it deploys and configures them. This last step
///      transfers each owner role, then reads it back.
///      Governance is the CrosschainGov precompile on Outbe, and its Interchain Account on any other chain. After
///      this step, an upgrade or a configuration change of an intent contract is a governance proposal.
///
/// Required env: `DEPLOYER_PK` (the current owner), `OUTBE_CHAIN_ID`.
/// Optional env: `AUCTION_ADDRESS`, `ESCROW_ADDRESS`, `SOLVER_ALLOCATOR_ADDRESS`, `ROUTER_ADDRESS`,
///   `ALLOCATOR_ADDRESS` (the contracts to hand over. An unset address is skipped), `OUTBE_RPC` (an
///   `rpc_endpoints` alias, default `outbe-testnet`).
contract HandOver is Script {
    error DomainTooLarge(uint256 chainId);
    error NotHandedOver(address target, address holder);
    error UnexpectedOwner(address target, address holder);

    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        IntentContracts memory owned = IntentContracts({
            auction: vm.envOr("AUCTION_ADDRESS", address(0)),
            escrow: vm.envOr("ESCROW_ADDRESS", address(0)),
            solverAllocator: vm.envOr("SOLVER_ALLOCATOR_ADDRESS", address(0)),
            router: vm.envOr("ROUTER_ADDRESS", address(0)),
            allocator: vm.envOr("ALLOCATOR_ADDRESS", address(0))
        });
        address governance = governanceOwner();

        vm.startBroadcast(deployerPk);
        handOver(vm.addr(deployerPk), governance, owned);
        vm.stopBroadcast();
    }

    /// @dev Governance on this chain: the precompile on Outbe, its Interchain Account elsewhere.
    function governanceOwner() public returns (address) {
        if (block.chainid == vm.envUint("OUTBE_CHAIN_ID")) return CROSSCHAIN_GOV;
        if (block.chainid > type(uint32).max) revert DomainTooLarge(block.chainid);
        return _governanceAccount(uint32(block.chainid));
    }

    /// @dev Requires an active broadcast. Each contract must still belong to the deployer or already belong to
    ///      governance.
    function handOver(address deployer, address governance, IntentContracts memory owned) public {
        address[5] memory all = _all(owned);
        for (uint256 i = 0; i < all.length; i++) {
            if (all[i] == address(0)) continue;
            address holder = Ownable(all[i]).owner();
            if (holder == deployer) {
                Ownable(all[i]).transferOwnership(governance);
            } else if (holder != governance) {
                revert UnexpectedOwner(all[i], holder);
            }
        }
        for (uint256 i = 0; i < all.length; i++) {
            if (all[i] == address(0)) continue;
            address holder = Ownable(all[i]).owner();
            if (holder != governance) revert NotHandedOver(all[i], holder);
        }
        console2.log("intent contracts handed over to governance:", governance);
    }

    /// @dev Asks the precompile on Outbe for its Interchain Account on `domain`. A fork cannot run the precompile,
    ///      so this is a raw `eth_call` to the Outbe node, as in `contracts/crosschain/script/4_HandOver.s.sol`.
    ///      `virtual` so a test harness can supply the account.
    function _governanceAccount(uint32 domain) internal virtual returns (address) {
        string memory params = string.concat(
            '[{"to":"',
            vm.toString(CROSSCHAIN_GOV),
            '","data":"',
            vm.toString(abi.encodeCall(ICrosschainGov.remoteAccount, (domain))),
            '"},"latest"]'
        );
        string memory outbe = vm.envOr("OUTBE_RPC", string("outbe-testnet"));
        return abi.decode(IRpcCheatcode(address(vm)).rpc(outbe, "eth_call", params), (address));
    }

    function _all(IntentContracts memory owned) private pure returns (address[5] memory) {
        return [owned.auction, owned.escrow, owned.solverAllocator, owned.router, owned.allocator];
    }
}
