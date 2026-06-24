// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";

import {DeployCreateXDeterministic} from "./0_DeployCreateX.s.sol";
import {DeployAdapters} from "./1_DeployAdapters.s.sol";
import {DeployBridge} from "./2_DeployBridge.s.sol";

/// @dev Full hub deploy on one chain: CreateX (if `CREATEX_ADDRESS` unset) → adapters (those whose endpoint env is
///      set) → bridge (with the active gateway). Cross-chain wiring is a separate step (3_ConfigureBridge), run after
///      every chain has been deployed.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `BRIDGE_OWNER`, at least one of `LZ_ENDPOINT` / `HYPERLANE_MAILBOX`.
/// Optional: `CREATEX_ADDRESS` (reused if set), `ACTIVE_GATEWAY` ("lz" | "hyperlane").
contract DeployAll is DeployCreateXDeterministic, DeployAdapters, DeployBridge {
    function run() public override(DeployCreateXDeterministic, DeployAdapters, DeployBridge) {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address owner = vm.envAddress("BRIDGE_OWNER");
        address deployer = vm.addr(deployerPk);

        address lzEndpoint = vm.envOr("LZ_ENDPOINT", address(0));
        address mailbox = vm.envOr("HYPERLANE_MAILBOX", address(0));
        require(lzEndpoint != address(0) || mailbox != address(0), "set LZ_ENDPOINT and/or HYPERLANE_MAILBOX");
        string memory active = vm.envOr("ACTIVE_GATEWAY", string(""));

        vm.startBroadcast(deployerPk);

        address createX = vm.envOr("CREATEX_ADDRESS", address(0));
        if (createX == address(0)) createX = deployCreateX(salt);
        console2.log("CreateX:", createX);

        address lz;
        address hl;
        if (lzEndpoint != address(0)) {
            lz = deployLayerZeroAdapter(createX, salt, deployer, owner, lzEndpoint);
            console2.log("LayerZeroGatewayAdapter:", lz);
        }
        if (mailbox != address(0)) {
            hl = deployHyperlaneAdapter(createX, salt, deployer, owner, mailbox);
            console2.log("HyperlaneGatewayAdapter:", hl);
        }

        address gateway = _pickGateway(active, lz, hl);
        address bridge = deployBridge(createX, salt, deployer, owner, gateway);
        console2.log("active gateway:", gateway);
        console2.log("ERC7786Bridge:", bridge);

        vm.stopBroadcast();
    }

    function _pickGateway(string memory active, address lz, address hl) internal pure returns (address) {
        bytes32 a = keccak256(bytes(active));
        if (a == keccak256("hyperlane")) {
            require(hl != address(0), "hyperlane adapter not deployed");
            return hl;
        }
        if (a == keccak256("lz")) {
            require(lz != address(0), "lz adapter not deployed");
            return lz;
        }
        return lz != address(0) ? lz : hl; // default: prefer LayerZero, else Hyperlane
    }
}
