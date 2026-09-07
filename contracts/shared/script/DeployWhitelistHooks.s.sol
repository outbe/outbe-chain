// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {IWhitelist, Whitelist} from "../src/Whitelist.sol";
import {V4SwapWhitelistHook, InfinitySwapWhitelistHook} from "../src/SwapWhitelistHook.sol";

/// @dev Deploys the {Whitelist} registry and the swap gates that read it, in one transaction batch
///      on one chain. Both hooks share the registry, so one add()/remove() governs every gated pool.
///
/// Required env vars:
///   DEPLOYER_PK              - deployer private key
/// Optional:
///   WHITELIST_OWNER          - registry admin; defaults to the deployer
///   WHITELIST_INITIAL        - csv of addresses to seed the registry with
///   V4_POOL_MANAGER          - deploys the Uniswap v4 gate when set
///   INFINITY_CL_POOL_MANAGER - deploys the PancakeSwap Infinity CL gate when set
contract DeployWhitelistHooks is Script {
    /// @dev Low 14 bits of a v4 hook address encode its callbacks; the manager calls exactly those.
    uint160 internal constant V4_ALL_HOOK_MASK = uint160((1 << 14) - 1);
    uint160 internal constant V4_BEFORE_SWAP_FLAG = uint160(1 << 7);

    /// @dev Salts scanned while mining a v4 address; ~1 in 16384 qualifies.
    uint256 internal constant V4_SALT_SCAN = 1_000_000;

    /// @notice Set by {run}; lets tests and follow-up scripts read what was deployed.
    Whitelist public registry;
    V4SwapWhitelistHook public v4Hook;
    InfinitySwapWhitelistHook public infinityHook;

    function run() public {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");

        vm.startBroadcast(deployerPrivateKey);
        deployAll(
            vm.envOr("WHITELIST_OWNER", vm.addr(deployerPrivateKey)),
            vm.envOr("WHITELIST_INITIAL", ",", new address[](0)),
            vm.envOr("V4_POOL_MANAGER", address(0)),
            vm.envOr("INFINITY_CL_POOL_MANAGER", address(0))
        );
        vm.stopBroadcast();
    }

    /// @notice Deploys the registry and the gates for whichever pool managers are given. Callers
    ///         wrap this in their own broadcast; `run` does that from env vars.
    function deployAll(address owner, address[] memory initial, address v4PoolManager, address infinityPoolManager)
        public
    {
        require(
            v4PoolManager != address(0) || infinityPoolManager != address(0),
            "set V4_POOL_MANAGER and/or INFINITY_CL_POOL_MANAGER"
        );

        // The constructor seeds the registry, so entries land regardless of who ends up as owner.
        registry = new Whitelist(owner, initial);
        console2.log("WHITELIST_ADDRESS=", address(registry));
        console2.log("  owner:", owner);
        console2.log("  seeded entries:", initial.length);

        if (v4PoolManager != address(0)) {
            v4Hook = V4SwapWhitelistHook(_deployV4Hook(v4PoolManager, registry));
            console2.log("V4_SWAP_WHITELIST_HOOK=", address(v4Hook));
        }
        if (infinityPoolManager != address(0)) {
            infinityHook = new InfinitySwapWhitelistHook(infinityPoolManager, registry);
            console2.log("INFINITY_SWAP_WHITELIST_HOOK=", address(infinityHook));
            console2.log("  poolKey.parameters bitmap:", infinityHook.getHooksRegistrationBitmap());
        }
    }

    /// @dev v4 reads a hook's callbacks off its address, so this one has to be mined onto an address
    ///      carrying beforeSwap and nothing else. Deployed through the canonical CREATE2 proxy so the
    ///      mined address holds both under `forge script` and in tests.
    function _deployV4Hook(address poolManager, IWhitelist registry) private returns (address hook) {
        require(CREATE2_FACTORY.code.length != 0, "Arachnid CREATE2 deployer not present on this chain");

        bytes memory initCode = v4InitCode(poolManager, address(registry));
        bytes32 salt;
        (salt, hook) = mineV4Salt(keccak256(initCode));

        (bool ok,) = CREATE2_FACTORY.call(abi.encodePacked(salt, initCode));
        require(ok && hook.code.length != 0, "V4SwapWhitelistHook deploy failed");
    }

    function v4InitCode(address poolManager, address registry) public pure returns (bytes memory) {
        return abi.encodePacked(type(V4SwapWhitelistHook).creationCode, abi.encode(poolManager, IWhitelist(registry)));
    }

    /// @notice First salt whose CREATE2 address carries the beforeSwap flag and nothing else - v4
    ///         calls every callback the address advertises, and this hook implements exactly one.
    function mineV4Salt(bytes32 initCodeHash) public pure returns (bytes32 salt, address hook) {
        for (uint256 i = 0; i < V4_SALT_SCAN; i++) {
            salt = bytes32(i);
            hook = vm.computeCreate2Address(salt, initCodeHash, CREATE2_FACTORY);
            if (uint160(hook) & V4_ALL_HOOK_MASK == V4_BEFORE_SWAP_FLAG) return (salt, hook);
        }
        revert("no v4 hook salt found");
    }
}
