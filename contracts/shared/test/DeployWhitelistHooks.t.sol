// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {DeployWhitelistHooks} from "../script/DeployWhitelistHooks.s.sol";

contract DeployWhitelistHooksTest is Test {
    uint160 internal constant ALL_HOOK_MASK = uint160((1 << 14) - 1);
    uint160 internal constant BEFORE_SWAP_FLAG = uint160(1 << 7);

    DeployWhitelistHooks internal deployScript;

    address internal owner = makeAddr("owner");
    address internal router = makeAddr("router");
    address internal v4PoolManager = makeAddr("v4PoolManager");
    address internal infinityPoolManager = makeAddr("infinityPoolManager");

    function setUp() public {
        deployScript = new DeployWhitelistHooks();
    }

    function _seed() internal view returns (address[] memory initial) {
        initial = new address[](1);
        initial[0] = router;
    }

    function test_DeployAll_WiresBothHooksToOneRegistry() public {
        deployScript.deployAll(owner, _seed(), v4PoolManager, infinityPoolManager);

        address registry = address(deployScript.registry());
        assertEq(deployScript.registry().owner(), owner, "registry owner");
        assertTrue(deployScript.registry().isWhitelisted(router), "seed not applied");

        assertEq(address(deployScript.v4Hook().registry()), registry, "v4 hook wired elsewhere");
        assertEq(deployScript.v4Hook().poolManager(), v4PoolManager, "v4 pool manager");
        assertEq(address(deployScript.infinityHook().registry()), registry, "infinity hook wired elsewhere");
        assertEq(deployScript.infinityHook().poolManager(), infinityPoolManager, "infinity pool manager");
    }

    /// @dev v4 calls exactly the callbacks the address advertises, so the mined address must carry
    ///      beforeSwap and no other flag - anything else means a callback this hook cannot answer.
    function test_DeployAll_V4HookAddressAdvertisesBeforeSwapOnly() public {
        deployScript.deployAll(owner, _seed(), v4PoolManager, address(0));
        assertEq(uint160(address(deployScript.v4Hook())) & ALL_HOOK_MASK, BEFORE_SWAP_FLAG, "wrong hook flags");
        assertEq(address(deployScript.infinityHook()), address(0), "infinity hook deployed unasked");
    }

    function test_DeployAll_RevertsWhenNoPoolManagerConfigured() public {
        vm.expectRevert("set V4_POOL_MANAGER and/or INFINITY_CL_POOL_MANAGER");
        deployScript.deployAll(owner, _seed(), address(0), address(0));
    }
}
