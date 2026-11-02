// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {CROSSCHAIN_GOV, ICrosschainGov} from "@precompiles/ICrosschainGov.sol";
import {BaseScript} from "./BaseScript.s.sol";

/// @title HandOver
/// @author Outbe
/// @notice Gives `DEFAULT_ADMIN_ROLE` of every intex proxy on this chain to governance (OIP-00325),
///         removes it from the deployer, and reads the roles back.
/// @dev Governance is CrosschainGov on Outbe and its Interchain Account on any other chain. Run it
///      last: after DeployOrigin or DeployTarget and the CD wiring of the system roles. After it,
///      an upgrade or a configuration change is a governance proposal (`upgradeContract`).
///      The script finds each proxy at its CREATE3 address and skips a proxy that is not deployed
///      here: Outbe has the OriginRouter and the target proxies, a remote target has the target
///      proxies only.
///      Env: DEPLOYER_PRIVATE_KEY (the current admin), CREATE3_FACTORY_ADDRESS, OUTBE_CHAIN_ID, and
///      on another chain OUTBE_RPC_URL (to read the Interchain Account from the Outbe node).
contract HandOver is BaseScript {
    error DomainTooLarge(uint256 chainId);
    error NotHandedOver(address proxy);

    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        Create3Factory factory = create3Factory();
        // Read before the broadcast: on a chain other than Outbe this asks the Outbe node.
        address governance = governanceOwner();

        string[6] memory prefixes =
            ["OriginRouter", "IntexNFT1155", "EscrowAdapter", "IntexAuction", "IntexNFT1155Bridge", "TargetRouter"];
        vm.startBroadcast(pk);
        for (uint256 i = 0; i < prefixes.length; i++) {
            address proxy = predictProxy(factory, deployer, prefixes[i]);
            if (proxy.code.length == 0) {
                console.log(string.concat(prefixes[i], ": not deployed here, skipped"));
                continue;
            }
            handOver(deployer, governance, proxy);
            console.log(string.concat(prefixes[i], " handed over:"), proxy);
        }
        vm.stopBroadcast();
        console.log("governance:", governance);
    }

    /// @dev Governance on this chain: CrosschainGov on Outbe, its Interchain Account elsewhere.
    function governanceOwner() public returns (address) {
        if (block.chainid == vm.envUint("OUTBE_CHAIN_ID")) return CROSSCHAIN_GOV;
        if (block.chainid > type(uint32).max) revert DomainTooLarge(block.chainid);
        return _governanceAccount(uint32(block.chainid));
    }

    /// @dev Requires an active broadcast from `deployer`. A second run on a handed-over proxy is a
    ///      no-op.
    function handOver(address deployer, address governance, address proxy) public {
        bytes32 admin = bytes32(0); // DEFAULT_ADMIN_ROLE
        IAccessControl c = IAccessControl(proxy);
        if (!c.hasRole(admin, governance)) c.grantRole(admin, governance);
        if (c.hasRole(admin, deployer)) c.renounceRole(admin, deployer);
        if (!c.hasRole(admin, governance) || c.hasRole(admin, deployer)) revert NotHandedOver(proxy);
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
        return abi.decode(vm.rpc(vm.envString("OUTBE_RPC_URL"), "eth_call", params), (address));
    }
}
