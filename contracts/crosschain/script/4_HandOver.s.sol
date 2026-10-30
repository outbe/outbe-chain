// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {HYPERLANE_CONTROLLER, IHyperlaneController} from "@precompiles/IHyperlaneController.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";

interface IEndpointDelegates {
    function delegates(address oapp) external view returns (address);
}

/// @dev Gives the hub and every deployed adapter to governance (OIP-00353). The deployer owns them
///      while it deploys and wires them. This last step transfers each owner role, and the
///      LayerZero OApp delegate, then reads them back.
///      Governance is the HyperlaneController precompile on Outbe, and its Interchain Account on
///      any other chain. After this step, a gateway switch, a new remote, a gas default or a pause
///      is a governance proposal.
///
/// Required env: `DEPLOYER_PK` (the current owner), `CONTRACT_SALT`, `CREATE3_FACTORY_ADDRESS`,
///   `OUTBE_CHAIN_ID`.
/// Optional env: `LZ_ENDPOINT`, `HYPERLANE_MAILBOX`, `WIRE_LOOPBACK` (select the adapters, as in the
///   deploy), `KEEP_DEPLOYER_AS_OWNER` (default `false`), `OUTBE_RPC` (an `rpc_endpoints` alias,
///   default `outbe-testnet`).
contract HandOver is Script {
    error DomainTooLarge(uint256 chainId);
    error NotHandedOver(address target, address holder);

    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        address deployer = vm.addr(deployerPk);

        address lz = vm.envOr("LZ_ENDPOINT", address(0)) != address(0)
            ? _predict(factory, salt, deployer, "LayerZeroGatewayAdapter")
            : address(0);
        address hl = vm.envOr("HYPERLANE_MAILBOX", address(0)) != address(0)
            ? _predict(factory, salt, deployer, "HyperlaneGatewayAdapter")
            : address(0);
        address lb =
            vm.envOr("WIRE_LOOPBACK", false) ? _predict(factory, salt, deployer, "LoopbackGatewayAdapter") : address(0);
        address governance = governanceOwner();

        vm.startBroadcast(deployerPk);
        handOver(governance, _predict(factory, salt, deployer, "ERC7786Bridge"), lz, hl, lb);
        vm.stopBroadcast();
    }

    /// @dev Governance on this chain: the controller on Outbe, its Interchain Account elsewhere.
    function governanceOwner() public returns (address) {
        if (vm.envOr("KEEP_DEPLOYER_AS_OWNER", false)) return vm.addr(vm.envUint("DEPLOYER_PK"));
        if (block.chainid == vm.envUint("OUTBE_CHAIN_ID")) return HYPERLANE_CONTROLLER;
        if (block.chainid > type(uint32).max) revert DomainTooLarge(block.chainid);
        return _controllerAccount(uint32(block.chainid));
    }

    /// @dev Requires an active broadcast whose sender owns every contract. A zero adapter address
    ///      means the adapter is not deployed.
    function handOver(address governance, address bridge, address lz, address hl, address lb) public {
        if (lz != address(0)) {
            // The endpoint accepts DVN and library changes from the delegate, so it moves first.
            LayerZeroGatewayAdapter(lz).setDelegate(governance);
            Ownable(lz).transferOwnership(governance);
            _require(lz, IEndpointDelegates(address(LayerZeroGatewayAdapter(lz).endpoint())).delegates(lz), governance);
        }
        address[3] memory owned = [bridge, hl, lb];
        for (uint256 i = 0; i < owned.length; i++) {
            if (owned[i] != address(0)) Ownable(owned[i]).transferOwnership(governance);
        }
        address[4] memory all = [bridge, lz, hl, lb];
        for (uint256 i = 0; i < all.length; i++) {
            if (all[i] != address(0)) _require(all[i], Ownable(all[i]).owner(), governance);
        }
        console2.log("handed over to governance:", governance);
    }

    /// @dev Asks the controller on Outbe for its Interchain Account on `domain`. A fork cannot run
    ///      the precompile, so this is a raw `eth_call` to the Outbe node, as in
    ///      `contracts/tokens/script/routes/BaseRoute.sol`. `virtual` so a test harness can supply
    ///      the account.
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

    function _require(address target, address holder, address governance) private pure {
        if (holder != governance) revert NotHandedOver(target, holder);
    }

    function _predict(address factory, string memory salt, address deployer, string memory label)
        internal
        view
        returns (address)
    {
        return Create3Factory(factory).predict(deployer, keccak256(abi.encodePacked(label, salt)));
    }
}
