// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";

/// @dev Deploys the gateway adapters as UUPS proxies via CREATE3 (same address on every chain).
///      `run()` deploys each adapter only if its endpoint env is set:
///      - LayerZero when `LZ_ENDPOINT` is present.
///      - Hyperlane when `HYPERLANE_MAILBOX` is present.
///
///      The deployer owns the adapters until `4_HandOver` gives them to governance.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT` (default `SALT_VERSION`), `CREATE3_FACTORY_ADDRESS`, and at least one of
/// `LZ_ENDPOINT` / `HYPERLANE_MAILBOX`.
contract DeployAdapters is Script {
    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        address deployer = vm.addr(deployerPk);

        address lzEndpoint = vm.envOr("LZ_ENDPOINT", address(0));
        address mailbox = vm.envOr("HYPERLANE_MAILBOX", address(0));
        require(lzEndpoint != address(0) || mailbox != address(0), "set LZ_ENDPOINT and/or HYPERLANE_MAILBOX");

        vm.startBroadcast(deployerPk);
        if (lzEndpoint != address(0)) {
            console2.log("LayerZeroGatewayAdapter:", deployLayerZeroAdapter(factory, salt, deployer, lzEndpoint));
        }
        if (mailbox != address(0)) {
            console2.log("HyperlaneGatewayAdapter:", deployHyperlaneAdapter(factory, salt, deployer, mailbox));
        }
        vm.stopBroadcast();
    }

    function deployLayerZeroAdapter(address factory, string memory salt, address owner, address lzEndpoint)
        public
        returns (address)
    {
        bytes32 saltHash = keccak256(abi.encodePacked("LayerZeroGatewayAdapter", salt));
        address impl = address(new LayerZeroGatewayAdapter(lzEndpoint));
        bytes memory init = abi.encodeCall(LayerZeroGatewayAdapter.initialize, (owner));
        return Create3Proxy.deploy(Create3Factory(factory), saltHash, impl, init);
    }

    function deployHyperlaneAdapter(address factory, string memory salt, address owner, address mailbox)
        public
        returns (address)
    {
        bytes32 saltHash = keccak256(abi.encodePacked("HyperlaneGatewayAdapter", salt));
        address impl = address(new HyperlaneGatewayAdapter(mailbox));
        bytes memory init = abi.encodeCall(HyperlaneGatewayAdapter.initialize, (owner));
        return Create3Proxy.deploy(Create3Factory(factory), saltHash, impl, init);
    }
}
