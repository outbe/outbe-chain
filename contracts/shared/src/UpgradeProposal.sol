// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {IUUPSUpgradeable} from "@shared/IUUPSUpgradeable.sol";

/// @title UpgradeProposal
/// @notice Prepares an upgrade of a project's UUPS proxies on every chain of its stack: it deploys
///         new implementations with the immutables read from the live proxies, and prints one
///         `upgradeContracts` payload for the Update vote.
/// @dev Anyone can run it: the implementations hold no rights, and only the vote upgrades.
///      Env: `PRIVATE_KEY` (any funded key; it sends the implementation deploys), `DEPLOYER_ADDRESS`
///      (the deployer of the stack, which fixes the CREATE3 addresses; default the key's address),
///      `RPC_URLS` (RPC URLs or `rpc_endpoints` aliases, comma-separated; unset means the
///      `--rpc-url` chain only) and `SOURCE_COMMIT` (the commit the implementations are built from,
///      required for a payload). A stack the deployer still owns, as one deployed with
///      `KEEP_DEPLOYER_AS_OWNER`, is upgraded directly, which needs the deployer's key. A stack where
///      the deployer owns some proxies and governance the others reverts.
abstract contract UpgradeProposal is Script {
    error MixedOwnership(address proxy);
    error MissingSourceCommit();
    error NotTheDeployerKey(address deployer);

    enum Mode {
        Unknown,
        Key,
        Vote
    }

    Mode internal mode;
    string[] internal upgrades;
    mapping(uint256 chainId => bool) internal prepared;

    /// @dev Every proxy of the project on the selected chain, deployed or not.
    function _proxies(address deployer) internal view virtual returns (address[] memory);

    /// @dev Deploys a new implementation for `proxy`, entry `index` of `_proxies`, with the
    ///      immutables of its current one.
    function _newImplementation(uint256 index, address proxy) internal virtual returns (address);

    /// @dev Whether `deployer` still holds the upgrade authority of `proxy`.
    function _deployerOwns(address proxy, address deployer) internal view virtual returns (bool);

    /// @dev The deployer of the stack. The CREATE3 addresses of its proxies depend on it.
    function _deployerAddress(uint256 pk) internal view virtual returns (address) {
        return vm.envOr("DEPLOYER_ADDRESS", vm.addr(pk));
    }

    /// @dev The commit the implementations are built from.
    function _sourceCommit() internal view virtual returns (string memory) {
        return vm.envOr("SOURCE_COMMIT", string(""));
    }

    function run() external {
        uint256 pk = vm.envUint("PRIVATE_KEY");
        string[] memory rpcs = vm.envOr("RPC_URLS", ",", new string[](0));
        if (rpcs.length == 0) {
            prepareChain(pk);
        } else {
            for (uint256 i = 0; i < rpcs.length; i++) {
                vm.createSelectFork(rpcs[i]);
                prepareChain(pk);
            }
        }
        printProposal();
    }

    /// @dev Upgrades with the key or records the upgrades of every deployed proxy on this chain. A
    ///      chain listed twice is prepared once.
    function prepareChain(uint256 pk) public {
        if (prepared[block.chainid]) return;
        prepared[block.chainid] = true;
        address deployer = _deployerAddress(pk);
        address[] memory proxies = _proxies(deployer);
        for (uint256 i = 0; i < proxies.length; i++) {
            if (proxies[i].code.length != 0) _setMode(proxies[i], deployer);
        }
        if (mode == Mode.Vote && bytes(_sourceCommit()).length == 0) revert MissingSourceCommit();
        if (mode == Mode.Key && vm.addr(pk) != deployer) revert NotTheDeployerKey(deployer);

        vm.startBroadcast(pk);
        for (uint256 i = 0; i < proxies.length; i++) {
            address proxy = proxies[i];
            if (proxy.code.length == 0) continue;
            address implementation = _newImplementation(i, proxy);
            if (mode == Mode.Key) {
                IUUPSUpgradeable(proxy).upgradeToAndCall(implementation, "");
                console.log("upgraded with the key:", proxy);
            } else {
                upgrades.push(_entry(proxy, implementation));
            }
        }
        vm.stopBroadcast();
    }

    /// @dev The `upgradeContracts` payload of the recorded upgrades, empty if there are none.
    function proposal() public view returns (string memory) {
        if (upgrades.length == 0) return "";
        string memory list = upgrades[0];
        for (uint256 i = 1; i < upgrades.length; i++) {
            list = string.concat(list, ",", upgrades[i]);
        }
        return string.concat('{"operation":"upgradeContracts","upgrades":[', list, "]}");
    }

    /// @dev Prints the payload and, on its own line, the source commit. The payload cannot carry
    ///      the commit: Update rejects unknown fields.
    function printProposal() public view {
        if (upgrades.length == 0) return;
        console.log(string.concat("upgradeContracts payload: ", proposal()));
        console.log(string.concat("source commit: ", _sourceCommit()));
    }

    function _setMode(address proxy, address deployer) internal {
        Mode found = _deployerOwns(proxy, deployer) ? Mode.Key : Mode.Vote;
        if (mode == Mode.Unknown) mode = found;
        else if (mode != found) revert MixedOwnership(proxy);
    }

    function _entry(address proxy, address implementation) internal view returns (string memory) {
        return string.concat(
            '{"chainId":',
            vm.toString(block.chainid),
            ',"proxy":"',
            vm.toString(proxy),
            '","implementation":"',
            vm.toString(implementation),
            '","callData":"0x"}'
        );
    }
}
