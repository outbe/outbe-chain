// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

import {SetPoolPrice, PoolKey} from "../../script/SetPoolPrice.s.sol";
import {USDC} from "../../src/canonical/USDC.sol";
import {BridgeableERC20} from "../../src/synthetic/BridgeableERC20.sol";

contract SetPoolPriceHarness is SetPoolPrice {
    function exposedParse(string memory price) external pure returns (uint256) {
        return _parseE18(price);
    }

    function exposedFormat(uint256 value) external pure returns (string memory) {
        return _formatE18(value);
    }

    function exposedSqrtPriceAt(Pool memory p, uint256 priceE18) external view returns (uint160) {
        return _sqrtPriceAt(p, priceE18);
    }

    function exposedPriceE18(Pool memory p, uint160 sqrtPriceX96) external view returns (uint256) {
        return _priceE18(p, sqrtPriceX96);
    }

    function exposedSqrtPrice(Pool memory p) external view returns (uint160) {
        return _sqrtPrice(p);
    }

    function exposedPlan(Pool memory p, address signer, uint256 targetE18) external returns (Plan memory) {
        return _plan(p, signer, targetE18);
    }

    function exposedLoadPool() external view returns (Pool memory) {
        return _loadPool();
    }

    function exposedExecute(Pool memory p, Plan memory plan, address signer) external {
        vm.startPrank(signer);
        _execute(p, plan, signer);
        vm.stopPrank();
    }
}

interface IPoolManager {
    function initialize(PoolKey calldata key, uint160 sqrtPriceX96) external returns (int24 tick);
}

interface IPoolModifyLiquidityTest {
    struct ModifyLiquidityParams {
        int24 tickLower;
        int24 tickUpper;
        int256 liquidityDelta;
        bytes32 salt;
    }

    function modifyLiquidity(PoolKey calldata key, ModifyLiquidityParams calldata params, bytes calldata hookData)
        external
        payable
        returns (int256 delta);
}

/// @dev The math tests run offline. The fork tests swap against the live Sepolia deployment and are
///      skipped when `SEPOLIA_RPC_URL` is not set.
contract SetPoolPriceTest is Test {
    // Sepolia: WCOEN (CONTRACT_SALT=v7.0.0), its mock USDC, and the V3 WCOEN/USDC 0.3% pool.
    address internal constant WCOEN = 0x3414f55f086Eb224135D9044299E6eBc6E841197;
    address internal constant MOCK_USDC = 0x61dE4c94974089F80790646e9d90094381e99264;
    address internal constant V3_POOL = 0xb028E7BB0c4138AE454FB28AEBe136b8a11eb445;
    address internal constant V4_POOL_MANAGER = 0xE03A1074c86CFeDd5C142C4F04F1a1536e203543;
    address internal constant V4_MODIFY_LIQUIDITY = 0x0C478023803a644c94c4CE1C1e7b9A087e411B0A;
    address internal constant V3_ROUTER = 0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E;
    address internal constant V4_SWAP_ROUTER = 0x9B6b46e2c869aa39918Db7f52f5557FE577B6eEe;
    address internal constant V4_STATE_VIEW = 0xE1Dd9c3fA50EDB962E442f60DfBc432e24537E4C;

    SetPoolPriceHarness internal harness;
    address internal signer = makeAddr("market-maker");

    function setUp() public {
        harness = new SetPoolPriceHarness();
    }

    modifier onSepoliaFork() {
        string memory rpc = vm.envOr("SEPOLIA_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
        } else {
            vm.createSelectFork(rpc);
            harness = new SetPoolPriceHarness();
        }
        _;
    }

    // ================================================ Offline =================================================

    function test_parse() public view {
        assertEq(harness.exposedParse("0.1"), 1e17);
        assertEq(harness.exposedParse("25"), 25e18);
        assertEq(harness.exposedParse("0.000351570500000000"), 351570500000000);
        assertEq(harness.exposedParse(".5"), 5e17);
        assertEq(harness.exposedParse("0.000000000000000001"), 1);
    }

    function test_parse_rejectsInvalidInput() public {
        string[7] memory bad = ["", ".", "0", "0.0", "1.2.3", "1,5", "0.0000000000000000001"];
        for (uint256 i = 0; i < bad.length; i++) {
            vm.expectRevert(abi.encodeWithSelector(SetPoolPrice.InvalidPrice.selector, bad[i]));
            harness.exposedParse(bad[i]);
        }
    }

    function test_format() public view {
        assertEq(harness.exposedFormat(1e17), "0.1");
        assertEq(harness.exposedFormat(25e18), "25");
        assertEq(harness.exposedFormat(438956250538772), "0.000438956250538772");
        assertEq(harness.exposedFormat(1), "0.000000000000000001");
    }

    /// @dev The live pool's slot0 on 2026-11-05, cross-checked with Python `Decimal`.
    function test_priceE18_matchesKnownPoolState() public {
        SetPoolPrice.Pool memory p = _offlinePool(true);
        assertEq(harness.exposedPriceE18(p, 1659931637753724218156), 438956250538772);
    }

    function testFuzz_sqrtPriceRoundTrip(uint256 priceE18, bool baseIsToken0) public {
        // 1e-9 to 1e9 quote per base.
        priceE18 = bound(priceE18, 1e9, 1e27);
        SetPoolPrice.Pool memory p = _offlinePool(baseIsToken0);
        uint160 sqrtPrice = harness.exposedSqrtPriceAt(p, priceE18);
        assertApproxEqRel(harness.exposedPriceE18(p, sqrtPrice), priceE18, 1e10); // 1e-8 relative
    }

    /// @dev A higher base price is a higher sqrt price only when the base token is token0.
    function test_sqrtPriceAt_followsTokenOrder() public {
        SetPoolPrice.Pool memory p0 = _offlinePool(true);
        SetPoolPrice.Pool memory p1 = _offlinePool(false);
        assertGt(harness.exposedSqrtPriceAt(p0, 1e17), harness.exposedSqrtPriceAt(p0, 1e15));
        assertLt(harness.exposedSqrtPriceAt(p1, 1e17), harness.exposedSqrtPriceAt(p1, 1e15));
    }

    // ================================================ Sepolia fork ============================================

    function test_v3_movesUpWithMintedQuoteThenDown() public onSepoliaFork {
        SetPoolPrice.Pool memory p = _v3Pool();
        _moveAndCheck(p, 1e17); // 0.1, buys WCOEN with freshly minted mock USDC
        deal(WCOEN, signer, 1e30);
        _moveAndCheck(p, 1e14); // 0.0001, sells WCOEN
    }

    /// @dev The pool address alone selects V3, and the WCOEN symbol selects the base token.
    function test_loadPool_v3FromAddressOnly() public onSepoliaFork {
        vm.setEnv("POOL_ADDRESS", vm.toString(V3_POOL));
        SetPoolPrice.Pool memory p = harness.exposedLoadPool();
        assertEq(uint8(p.protocol), uint8(SetPoolPrice.Protocol.UniswapV3));
        assertEq(p.base, WCOEN);
        assertEq(p.quote, MOCK_USDC);
        assertEq(p.fee, 3000);
        assertEq(p.router, V3_ROUTER);
    }

    function test_plan_detectsMintableInput() public onSepoliaFork {
        SetPoolPrice.Pool memory p = _v3Pool();
        assertTrue(harness.exposedPlan(p, signer, 1e17).mintable, "mock USDC has a public mint");
        assertFalse(harness.exposedPlan(p, signer, 1e14).mintable, "WCOEN has no public mint");
    }

    function test_v3_revertsWithoutBalance() public onSepoliaFork {
        SetPoolPrice.Pool memory p = _v3Pool();
        SetPoolPrice.Plan memory plan = harness.exposedPlan(p, signer, 1e14);
        assertGt(plan.amountIn, 0);
        vm.expectRevert(abi.encodeWithSelector(SetPoolPrice.InsufficientBalance.selector, WCOEN, 0, plan.amountIn));
        harness.exposedExecute(p, plan, signer);
    }

    function test_v4_movesUpThenDown() public onSepoliaFork {
        SetPoolPrice.Pool memory p = _v4Pool();
        _initializeV4(p, 438956250538772); // the price of the live V3 pool
        _moveAndCheck(p, 1e17);
        deal(WCOEN, signer, 1e30);
        _moveAndCheck(p, 1e14);
    }

    function test_atTarget_noSwap() public onSepoliaFork {
        SetPoolPrice.Pool memory p = _v3Pool();
        _moveAndCheck(p, 2e16);
        SetPoolPrice.Plan memory plan = harness.exposedPlan(p, signer, 2e16);
        assertEq(plan.amountIn, 0);
    }

    // ================================================ Helpers =================================================

    function _moveAndCheck(SetPoolPrice.Pool memory p, uint256 targetE18) internal {
        SetPoolPrice.Plan memory plan = harness.exposedPlan(p, signer, targetE18);
        assertGt(plan.amountIn, 0, "no swap planned");
        // The plan restores the state, so the pool has not moved yet.
        assertEq(harness.exposedSqrtPrice(p), plan.sqrtPriceBefore);

        uint256 inBefore = IERC20(plan.tokenIn).balanceOf(signer);
        harness.exposedExecute(p, plan, signer);

        uint160 sqrtAfter = harness.exposedSqrtPrice(p);
        assertEq(sqrtAfter, plan.sqrtPriceAfter, "real swap differs from the plan");
        assertApproxEqRel(harness.exposedPriceE18(p, sqrtAfter), targetE18, 1e12, "price is not at the target");
        // The router takes exactly the planned input. A mint first raises a smaller balance to that input.
        uint256 funded = inBefore > plan.amountIn ? inBefore : plan.amountIn;
        assertEq(IERC20(plan.tokenIn).balanceOf(signer), funded - plan.amountIn);
    }

    function _offlinePool(bool baseIsToken0) internal returns (SetPoolPrice.Pool memory p) {
        p.base = address(new BridgeableERC20("Wrapped COEN", "WCOEN", 18, address(1)));
        p.quote = address(new USDC());
        (p.token0, p.token1) = baseIsToken0 ? (p.base, p.quote) : (p.quote, p.base);
    }

    function _v3Pool() internal pure returns (SetPoolPrice.Pool memory p) {
        p.protocol = SetPoolPrice.Protocol.UniswapV3;
        p.base = WCOEN;
        p.quote = MOCK_USDC;
        (p.token0, p.token1) = (WCOEN, MOCK_USDC);
        p.fee = 3000;
        p.pool = V3_POOL;
        p.router = V3_ROUTER;
    }

    /// @dev A pool key that no live pool uses, so the fork can initialize it.
    function _v4Pool() internal pure returns (SetPoolPrice.Pool memory p) {
        p.protocol = SetPoolPrice.Protocol.UniswapV4;
        p.base = WCOEN;
        p.quote = MOCK_USDC;
        (p.token0, p.token1) = (WCOEN, MOCK_USDC);
        p.fee = 4321;
        p.tickSpacing = 7;
        p.router = V4_SWAP_ROUTER;
        p.stateView = V4_STATE_VIEW;
    }

    /// @dev Initializes the V4 pool and adds full-range liquidity.
    function _initializeV4(SetPoolPrice.Pool memory p, uint256 priceE18) internal {
        PoolKey memory key = PoolKey(p.token0, p.token1, p.fee, p.tickSpacing, p.hooks);
        IPoolManager(V4_POOL_MANAGER).initialize(key, harness.exposedSqrtPriceAt(p, priceE18));

        address lp = makeAddr("lp");
        deal(WCOEN, lp, 1e30);
        deal(MOCK_USDC, lp, 1e15);
        vm.startPrank(lp);
        IERC20(WCOEN).approve(V4_MODIFY_LIQUIDITY, type(uint256).max);
        IERC20(MOCK_USDC).approve(V4_MODIFY_LIQUIDITY, type(uint256).max);
        int24 maxTick = 887272 / p.tickSpacing * p.tickSpacing;
        IPoolModifyLiquidityTest(V4_MODIFY_LIQUIDITY)
            .modifyLiquidity(
                key,
                IPoolModifyLiquidityTest.ModifyLiquidityParams({
                    tickLower: -maxTick, tickUpper: maxTick, liquidityDelta: 1e18, salt: bytes32(0)
                }),
                ""
            );
        vm.stopPrank();
    }
}
