// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {DeployBase, Hub} from "./DeployBase.sol";

/// @dev The delegate getter of the LayerZero EndpointV2. `ILayerZeroEndpointV2` does not declare it.
interface ILayerZeroDelegates {
    function delegates(address oapp) external view returns (address);
}

/// @dev Gives the hub and every deployed adapter to governance. This is the last deploy step.
///      Until it runs, the deployer can make the hub deliver a forged message, which a token bridge
///      accepts and mints on.
///
///      The script finds each contract at its CREATE3 address. It includes an adapter only if its
///      endpoint env is set, as 1_DeployAdapters does. On the LayerZero adapter, it sets the OApp
///      delegate to governance before it transfers the owner. Otherwise the deployer keeps the
///      DVN and library configuration on the endpoint. A contract that governance already owns is
///      skipped, so a run that stopped partway can run again.
///
///      At the end, the script reads every owner and the LayerZero delegate back. It reverts if
///      one of them is not governance.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATE3_FACTORY_ADDRESS`, `OUTBE_CHAIN_ID`.
/// Optional: `LZ_ENDPOINT`, `HYPERLANE_MAILBOX`, `WIRE_LOOPBACK`, `OUTBE_RPC` (an `rpc_endpoints`
///   alias, default `outbe-testnet`).
contract HandOver is DeployBase {
    error MissingCode(string name, address target);
    error OwnerNotGovernance(string name, address target, address owner, address governance);
    error DelegateNotGovernance(address lzAdapter, address delegate, address governance);

    function run() public virtual {
        vm.startBroadcast(_pk());
        _handOverDeployed();
        vm.stopBroadcast();
    }

    /// @dev Hands over the contracts that the env enables. Opens no broadcast, so a test can call
    ///      it directly.
    function _handOverDeployed() internal {
        handOver(_deployedHub(), _governanceOwner());
    }

    /// @dev Transfers every owner role of `hub` to `governance`, then checks the result. Requires
    ///      an active broadcast whose sender owns each contract that governance does not own yet.
    function handOver(Hub memory hub, address governance) public {
        console2.log("governance owner:", governance);
        if (hub.lz != address(0)) {
            _requireCode("LayerZeroGatewayAdapter", hub.lz);
            LayerZeroGatewayAdapter lz = LayerZeroGatewayAdapter(hub.lz);
            if (lz.owner() != governance) {
                lz.setDelegate(governance);
                lz.transferOwnership(governance);
            }
        }
        _transfer("HyperlaneGatewayAdapter", hub.hl, governance);
        _transfer("LoopbackGatewayAdapter", hub.lb, governance);
        _transfer("ERC7786Bridge", hub.bridge, governance);

        checkHandOver(hub, governance);
        console2.log("=== Hand-over complete ===");
    }

    /// @dev Reverts unless governance owns the hub and each deployed adapter, and is the
    ///      LayerZero delegate.
    function checkHandOver(Hub memory hub, address governance) public view {
        _checkOwner("ERC7786Bridge", hub.bridge, governance);
        _checkOwner("LayerZeroGatewayAdapter", hub.lz, governance);
        _checkOwner("HyperlaneGatewayAdapter", hub.hl, governance);
        _checkOwner("LoopbackGatewayAdapter", hub.lb, governance);
        if (hub.lz != address(0)) {
            address endpoint = address(LayerZeroGatewayAdapter(hub.lz).endpoint());
            address delegate = ILayerZeroDelegates(endpoint).delegates(hub.lz);
            if (delegate != governance) revert DelegateNotGovernance(hub.lz, delegate, governance);
        }
    }

    /// @dev The CREATE3 addresses of the hub and of each adapter that the env enables.
    function _deployedHub() internal view returns (Hub memory hub) {
        string memory salt = vm.envString("CONTRACT_SALT");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        address deployer = _deployer();

        hub.bridge = _compute(factory, salt, deployer, "ERC7786Bridge");
        if (vm.envOr("LZ_ENDPOINT", address(0)) != address(0)) {
            hub.lz = _compute(factory, salt, deployer, "LayerZeroGatewayAdapter");
        }
        if (vm.envOr("HYPERLANE_MAILBOX", address(0)) != address(0)) {
            hub.hl = _compute(factory, salt, deployer, "HyperlaneGatewayAdapter");
        }
        if (vm.envOr("WIRE_LOOPBACK", false)) {
            hub.lb = _compute(factory, salt, deployer, "LoopbackGatewayAdapter");
        }
    }

    function _transfer(string memory name, address target, address governance) internal {
        if (target == address(0)) return;
        _requireCode(name, target);
        if (Ownable(target).owner() != governance) Ownable(target).transferOwnership(governance);
    }

    function _checkOwner(string memory name, address target, address governance) internal view {
        if (target == address(0)) return;
        address owner = Ownable(target).owner();
        if (owner != governance) revert OwnerNotGovernance(name, target, owner, governance);
    }

    function _requireCode(string memory name, address target) internal view {
        if (target.code.length == 0) revert MissingCode(name, target);
    }
}
