// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {IHyperlaneController} from "@precompiles/IHyperlaneController.sol";

/// @dev The hub and the adapters of one chain. A zero address means that the deploy does not
///      include that adapter.
struct Hub {
    address bridge;
    address lz;
    address hl;
    address lb;
}

/// @dev Shared part of the crosschain deploy scripts: the deployer, the CREATE3 addresses and the
///      governance owner. The deployer owns the hub and the adapters while it deploys and wires
///      them. The last step (4_HandOver) gives every owner role to governance.
abstract contract DeployBase is Script {
    /// @dev The HyperlaneController precompile. Governance votes drive it, so it owns the hub and
    ///      the adapters on Outbe.
    address internal constant HYPERLANE_CONTROLLER = 0x000000000000000000000000000000000000EE14;

    error DomainTooLarge(uint256 chainId);

    function _pk() internal view returns (uint256) {
        return vm.envUint("DEPLOYER_PK");
    }

    /// @dev `virtual` so a test harness can act as the deployer. The deployer is part of every
    ///      CREATE3 salt and is the owner until the hand-over.
    function _deployer() internal view virtual returns (address) {
        return vm.addr(_pk());
    }

    /// @dev Governance owns the hub and the adapters. On Outbe that is the HyperlaneController
    ///      precompile. On another chain it is the Interchain Account of the controller there.
    function _governanceOwner() internal returns (address) {
        return _isOutbe() ? HYPERLANE_CONTROLLER : _controllerAccount(_toDomain(block.chainid));
    }

    /// @dev Asks the controller on Outbe for its Interchain Account on `domain`. A fork cannot run
    ///      the precompile, so this is a raw `eth_call` to the Outbe node. `virtual` so a test
    ///      harness, which has no Outbe node, can supply the account.
    function _controllerAccount(uint32 domain) internal virtual returns (address) {
        string memory params = string.concat(
            '[{"to":"',
            vm.toString(HYPERLANE_CONTROLLER),
            '","data":"',
            vm.toString(abi.encodeCall(IHyperlaneController.remoteAccount, (domain))),
            '"},"latest"]'
        );
        string memory outbe = vm.envOr("OUTBE_RPC", string("outbe-testnet"));
        return abi.decode(vm.rpc(outbe, "eth_call", params), (address));
    }

    /// @dev The chain is Outbe iff it is the chain the operator declared as Outbe. `envUint`, not
    ///      `envOr`: an unset value must not silently mean "not Outbe".
    function _isOutbe() internal view returns (bool) {
        return block.chainid == vm.envUint("OUTBE_CHAIN_ID");
    }

    function _toDomain(uint256 chainId) internal pure returns (uint32) {
        if (chainId > type(uint32).max) revert DomainTooLarge(chainId);
        return uint32(chainId);
    }

    function _compute(address factory, string memory salt, address deployer, string memory label)
        internal
        view
        returns (address)
    {
        return Create3Factory(factory).predict(deployer, keccak256(abi.encodePacked(label, salt)));
    }
}
