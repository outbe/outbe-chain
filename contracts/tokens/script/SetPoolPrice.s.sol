// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {StdCheats} from "forge-std/StdCheats.sol";
import {console2} from "forge-std/console2.sol";

import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {Math} from "@openzeppelin/contracts/utils/math/Math.sol";
import {Strings} from "@openzeppelin/contracts/utils/Strings.sol";

interface IUniswapV3Pool {
    function token0() external view returns (address);
    function token1() external view returns (address);
    function fee() external view returns (uint24);
    function slot0() external view returns (uint160 sqrtPriceX96, int24, uint16, uint16, uint16, uint8, bool);
}

/// @dev Uniswap SwapRouter02. Its `exactInputSingle` has no deadline field.
interface ISwapRouter02 {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

/// @dev Uniswap V4 pool key. The field order is the ABI order that the pool id hashes.
struct PoolKey {
    address currency0;
    address currency1;
    uint24 fee;
    int24 tickSpacing;
    address hooks;
}

interface IStateView {
    function poolManager() external view returns (address);
    function getSlot0(bytes32 poolId)
        external
        view
        returns (uint160 sqrtPriceX96, int24 tick, uint24 protocolFee, uint24 lpFee);
}

/// @dev Uniswap V4 PoolSwapTest router. It pulls the input token from the caller with `transferFrom`.
interface IPoolSwapTest {
    struct SwapParams {
        bool zeroForOne;
        int256 amountSpecified;
        uint160 sqrtPriceLimitX96;
    }

    struct TestSettings {
        bool takeClaims;
        bool settleUsingBurn;
    }

    function manager() external view returns (address);
    function swap(
        PoolKey calldata key,
        SwapParams calldata params,
        TestSettings calldata settings,
        bytes calldata hookData
    ) external payable returns (int256 delta);
}

interface IMintable {
    function mint(address to, uint256 amount) external;
}

/// @dev Moves a Uniswap V3 or V4 pool to a target price with one swap. The swap has an exact input and a price
///      limit at the target, so the pool stops at the target and the router takes only the input that the move
///      needs. The script first runs the swap on a local copy of the state with an unlimited balance. That run
///      gives the exact input amount. The real swap then spends that amount.
///
///      The price is quote per base in whole tokens, for example 0.1 USDC per WCOEN. The direction follows from
///      the current price: a higher target buys base with quote, a lower target sells base for quote.
///
///      Run without `--broadcast` to preview the swap.
///
///      If the input token has a public `mint` (a mock token), the script mints the missing input. Otherwise a
///      missing input stops the script before any transaction.
///
/// Required env: `MM_PK`, `TARGET_PRICE` (decimal, at most 18 fractional digits), and the pool:
///   V3: `POOL_ADDRESS`. The pool gives the tokens and the fee.
///   V4: `BASE_TOKEN`, `QUOTE_TOKEN`, `POOL_FEE` (hundredths of a bip), `POOL_TICK_SPACING`, `POOL_HOOKS`
///   (default zero). A V4 pool has no address, and its id does not give back the key.
/// Optional env: `BASE_TOKEN` for V3, default the pool token with the symbol WCOEN. `V3_ROUTER`,
///   `V4_SWAP_ROUTER`, `V4_STATE_VIEW` have Sepolia defaults and are required on other chains.
contract SetPoolPrice is Script, StdCheats {
    enum Protocol {
        UniswapV3,
        UniswapV4
    }

    struct Pool {
        Protocol protocol;
        address base;
        address quote;
        address token0;
        address token1;
        uint24 fee;
        /// @dev V3 only: the pool contract.
        address pool;
        /// @dev SwapRouter02 for V3, PoolSwapTest for V4.
        address router;
        /// @dev V4 only.
        int24 tickSpacing;
        address hooks;
        address stateView;
    }

    struct Plan {
        bool zeroForOne;
        address tokenIn;
        address tokenOut;
        uint160 sqrtPriceBefore;
        uint160 sqrtPriceTarget;
        uint160 sqrtPriceAfter;
        uint256 amountIn;
        uint256 amountOut;
        /// @dev The input token has a public `mint`.
        bool mintable;
    }

    uint256 internal constant SEPOLIA = 11155111;
    address internal constant SEPOLIA_V3_ROUTER = 0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E;
    address internal constant SEPOLIA_V4_SWAP_ROUTER = 0x9B6b46e2c869aa39918Db7f52f5557FE577B6eEe;
    address internal constant SEPOLIA_V4_STATE_VIEW = 0xE1Dd9c3fA50EDB962E442f60DfBc432e24537E4C;

    /// @dev The sqrt price bounds of Uniswap TickMath. A price limit must be strictly inside them.
    uint160 internal constant MIN_SQRT_PRICE = 4295128739;
    uint160 internal constant MAX_SQRT_PRICE = 1461446703485210103287273052203988822378723970342;
    uint256 internal constant Q96 = 1 << 96;
    uint256 internal constant Q192 = 1 << 192;
    /// @dev The balance of the local run. The price limit, not this amount, ends the swap.
    uint256 internal constant SIMULATION_BALANCE = type(uint128).max;

    error BaseTokenRequired(address token0, address token1);
    error PoolTokenMismatch(address token0, address token1);
    error ManagerMismatch(address stateViewManager, address routerManager);
    error PoolNotInitialized();
    error InvalidPrice(string price);
    error TargetOutOfRange(uint256 priceE18);
    error InsufficientBalance(address token, uint256 balance, uint256 required);

    function run() external {
        uint256 pk = vm.envUint("MM_PK");
        address signer = vm.addr(pk);
        Pool memory p = _loadPool();
        uint256 targetE18 = _parseE18(vm.envString("TARGET_PRICE"));

        Plan memory plan = _plan(p, signer, targetE18);
        _logPlan(p, plan, targetE18);
        if (plan.amountIn == 0) return;

        vm.startBroadcast(pk);
        _execute(p, plan, signer);
        vm.stopBroadcast();

        console2.log("price now:", _formatE18(_priceE18(p, _sqrtPrice(p))));
    }

    // ==================================================== Pool ====================================================

    /// @dev `POOL_ADDRESS` selects V3. Without it the env describes a V4 pool key.
    function _loadPool() internal view returns (Pool memory p) {
        p.pool = vm.envOr("POOL_ADDRESS", address(0));
        if (p.pool != address(0)) {
            p.protocol = Protocol.UniswapV3;
            _requireCode(p.pool);
            p.token0 = IUniswapV3Pool(p.pool).token0();
            p.token1 = IUniswapV3Pool(p.pool).token1();
            p.fee = IUniswapV3Pool(p.pool).fee();
            p.router = _chainAddress("V3_ROUTER", SEPOLIA_V3_ROUTER);
            p.base = vm.envOr("BASE_TOKEN", _coenToken(p.token0, p.token1));
            p.quote = p.base == p.token0 ? p.token1 : p.token0;
        } else {
            p.protocol = Protocol.UniswapV4;
            p.base = vm.envAddress("BASE_TOKEN");
            p.quote = vm.envAddress("QUOTE_TOKEN");
            (p.token0, p.token1) = p.base < p.quote ? (p.base, p.quote) : (p.quote, p.base);
            p.fee = uint24(vm.envUint("POOL_FEE"));
            p.tickSpacing = int24(vm.envInt("POOL_TICK_SPACING"));
            p.hooks = vm.envOr("POOL_HOOKS", address(0));
            p.router = _chainAddress("V4_SWAP_ROUTER", SEPOLIA_V4_SWAP_ROUTER);
            p.stateView = _chainAddress("V4_STATE_VIEW", SEPOLIA_V4_STATE_VIEW);
        }
        _checkPool(p);
    }

    /// @dev The pool token with the symbol WCOEN. Any other pair needs `BASE_TOKEN`.
    function _coenToken(address token0, address token1) internal view returns (address) {
        if (_eq(IERC20Metadata(token0).symbol(), "WCOEN")) return token0;
        if (_eq(IERC20Metadata(token1).symbol(), "WCOEN")) return token1;
        revert BaseTokenRequired(token0, token1);
    }

    function _checkPool(Pool memory p) internal view {
        bool sameTokens = (p.token0 == p.base && p.token1 == p.quote) || (p.token0 == p.quote && p.token1 == p.base);
        if (!sameTokens || p.base == p.quote) revert PoolTokenMismatch(p.token0, p.token1);
        _requireCode(p.router);
        if (p.protocol == Protocol.UniswapV4) {
            _requireCode(p.stateView);
            address stateViewManager = IStateView(p.stateView).poolManager();
            address routerManager = IPoolSwapTest(p.router).manager();
            if (stateViewManager != routerManager) revert ManagerMismatch(stateViewManager, routerManager);
        }
        // Reverts with PoolNotInitialized for a pool that does not exist.
        _sqrtPrice(p);
    }

    function _key(Pool memory p) internal pure returns (PoolKey memory) {
        return PoolKey(p.token0, p.token1, p.fee, p.tickSpacing, p.hooks);
    }

    function _poolId(Pool memory p) internal pure returns (bytes32) {
        return keccak256(abi.encode(_key(p)));
    }

    function _sqrtPrice(Pool memory p) internal view returns (uint160 sqrtPriceX96) {
        if (p.protocol == Protocol.UniswapV3) {
            (sqrtPriceX96,,,,,,) = IUniswapV3Pool(p.pool).slot0();
        } else {
            (sqrtPriceX96,,,) = IStateView(p.stateView).getSlot0(_poolId(p));
        }
        if (sqrtPriceX96 == 0) revert PoolNotInitialized();
    }

    // ==================================================== Swap ====================================================

    /// @dev Runs the swap on a local copy of the state and restores the state afterwards. A zero `amountIn`
    ///      means that the pool is already at the target.
    function _plan(Pool memory p, address signer, uint256 targetE18) internal returns (Plan memory plan) {
        plan.sqrtPriceBefore = _sqrtPrice(p);
        plan.sqrtPriceTarget = _sqrtPriceAt(p, targetE18);
        if (plan.sqrtPriceTarget == plan.sqrtPriceBefore) {
            plan.sqrtPriceAfter = plan.sqrtPriceBefore;
            return plan;
        }
        // A lower sqrt price is a lower price of token0. The swap sells token0 to get there.
        plan.zeroForOne = plan.sqrtPriceTarget < plan.sqrtPriceBefore;
        (plan.tokenIn, plan.tokenOut) = plan.zeroForOne ? (p.token0, p.token1) : (p.token1, p.token0);

        uint256 snapshot = vm.snapshotState();
        vm.prank(signer);
        try IMintable(plan.tokenIn).mint(signer, 1) {
            plan.mintable = true;
        } catch {}
        deal(plan.tokenIn, signer, SIMULATION_BALANCE);
        uint256 outBefore = IERC20Metadata(plan.tokenOut).balanceOf(signer);
        vm.startPrank(signer);
        IERC20Metadata(plan.tokenIn).approve(p.router, SIMULATION_BALANCE);
        _swap(p, plan, SIMULATION_BALANCE, signer);
        vm.stopPrank();
        plan.amountIn = SIMULATION_BALANCE - IERC20Metadata(plan.tokenIn).balanceOf(signer);
        plan.amountOut = IERC20Metadata(plan.tokenOut).balanceOf(signer) - outBefore;
        plan.sqrtPriceAfter = _sqrtPrice(p);
        vm.revertToState(snapshot);
    }

    /// @dev The caller sets the sender: `vm.startBroadcast` in `run`, `vm.startPrank` in a test.
    function _execute(Pool memory p, Plan memory plan, address signer) internal {
        uint256 balance = IERC20Metadata(plan.tokenIn).balanceOf(signer);
        if (balance < plan.amountIn) {
            if (!plan.mintable) revert InsufficientBalance(plan.tokenIn, balance, plan.amountIn);
            IMintable(plan.tokenIn).mint(signer, plan.amountIn - balance);
        }
        IERC20Metadata(plan.tokenIn).approve(p.router, plan.amountIn);
        _swap(p, plan, plan.amountIn, signer);
    }

    /// @dev The minimum output is zero because the price limit bounds the execution price: no part of the swap
    ///      fills beyond the target.
    function _swap(Pool memory p, Plan memory plan, uint256 amountIn, address recipient) internal {
        if (p.protocol == Protocol.UniswapV3) {
            ISwapRouter02(p.router)
                .exactInputSingle(
                    ISwapRouter02.ExactInputSingleParams({
                        tokenIn: plan.tokenIn,
                        tokenOut: plan.tokenOut,
                        fee: p.fee,
                        recipient: recipient,
                        amountIn: amountIn,
                        amountOutMinimum: 0,
                        sqrtPriceLimitX96: plan.sqrtPriceTarget
                    })
                );
        } else {
            // PoolSwapTest pays the output to the caller, which is the recipient here.
            IPoolSwapTest(p.router)
                .swap(
                    _key(p),
                    IPoolSwapTest.SwapParams({
                        zeroForOne: plan.zeroForOne,
                        // A negative amount is an exact input.
                        amountSpecified: -int256(amountIn),
                        sqrtPriceLimitX96: plan.sqrtPriceTarget
                    }),
                    IPoolSwapTest.TestSettings({takeClaims: false, settleUsingBurn: false}),
                    ""
                );
        }
    }

    // ==================================================== Price ===================================================

    /// @dev The sqrt price (Q64.96, token1 per token0 in raw units) at which one whole base token costs
    ///      `priceE18 / 1e18` whole quote tokens. The math runs in the base-per-quote orientation and inverts
    ///      the result when the base token is token1. Thus the intermediate stays small for any token order.
    function _sqrtPriceAt(Pool memory p, uint256 priceE18) internal view returns (uint160) {
        uint256 ratioX192 =
            Math.mulDiv(priceE18 * 10 ** _decimals(p.quote), Q192, 10 ** (18 + uint256(_decimals(p.base))));
        uint256 sqrtBase = Math.sqrt(ratioX192);
        if (sqrtBase == 0) revert TargetOutOfRange(priceE18);
        uint256 sqrtPrice = p.base == p.token0 ? sqrtBase : Q192 / sqrtBase;
        if (sqrtPrice <= MIN_SQRT_PRICE || sqrtPrice >= MAX_SQRT_PRICE) revert TargetOutOfRange(priceE18);
        return uint160(sqrtPrice);
    }

    /// @dev The inverse of `_sqrtPriceAt`: whole quote tokens per whole base token, scaled by 1e18.
    function _priceE18(Pool memory p, uint160 sqrtPriceX96) internal view returns (uint256) {
        uint256 sqrtBase = p.base == p.token0 ? sqrtPriceX96 : Q192 / sqrtPriceX96;
        // Scale before the second multiplication. A small sqrt price otherwise loses its digits in a Q96 ratio.
        uint256 scaled = Math.mulDiv(sqrtBase, 10 ** (18 + uint256(_decimals(p.base))), Q96);
        return Math.mulDiv(sqrtBase, scaled, Q96 * 10 ** _decimals(p.quote));
    }

    function _decimals(address token) internal view returns (uint8) {
        return IERC20Metadata(token).decimals();
    }

    /// @dev Parses a positive decimal such as `0.1` or `25` into a value scaled by 1e18.
    function _parseE18(string memory price) internal pure returns (uint256 value) {
        bytes memory b = bytes(price);
        bool dot;
        uint256 digits;
        uint256 fractional;
        for (uint256 i = 0; i < b.length; i++) {
            if (b[i] == ".") {
                if (dot) revert InvalidPrice(price);
                dot = true;
                continue;
            }
            if (b[i] < "0" || b[i] > "9") revert InvalidPrice(price);
            if (dot && ++fractional > 18) revert InvalidPrice(price);
            digits++;
            value = value * 10 + (uint8(b[i]) - 48);
        }
        if (digits == 0) revert InvalidPrice(price);
        value *= 10 ** (18 - fractional);
        if (value == 0) revert InvalidPrice(price);
    }

    /// @dev Formats a value scaled by 1e18 as a decimal without trailing zeros.
    function _formatE18(uint256 value) internal pure returns (string memory) {
        uint256 fraction = value % 1e18;
        if (fraction == 0) return Strings.toString(value / 1e18);
        bytes memory digits = bytes(Strings.toString(fraction + 1e18));
        uint256 end = digits.length;
        while (digits[end - 1] == "0") end--;
        bytes memory fractionDigits = new bytes(end - 1);
        for (uint256 i = 1; i < end; i++) {
            fractionDigits[i - 1] = digits[i];
        }
        return string.concat(Strings.toString(value / 1e18), ".", string(fractionDigits));
    }

    // ==================================================== Helpers =================================================

    /// @dev On Sepolia the env value overrides the default. On other chains the env value is required, so a
    ///      Sepolia address cannot be used by mistake.
    function _chainAddress(string memory name, address sepoliaDefault) internal view returns (address) {
        return block.chainid == SEPOLIA ? vm.envOr(name, sepoliaDefault) : vm.envAddress(name);
    }

    function _requireCode(address target) internal view {
        require(target.code.length != 0, string.concat("no code at ", vm.toString(target)));
    }

    function _eq(string memory a, string memory b) internal pure returns (bool) {
        return keccak256(bytes(a)) == keccak256(bytes(b));
    }

    function _logPlan(Pool memory p, Plan memory plan, uint256 targetE18) internal view {
        string memory base = IERC20Metadata(p.base).symbol();
        string memory quote = IERC20Metadata(p.quote).symbol();
        console2.log(
            string.concat("pool: ", p.protocol == Protocol.UniswapV3 ? "uniswap_v3 " : "uniswap_v4 ", base, "/", quote)
        );
        if (p.protocol == Protocol.UniswapV3) console2.log("  address:", p.pool);
        else console2.logBytes32(_poolId(p));
        console2.log("price before:", _formatE18(_priceE18(p, plan.sqrtPriceBefore)));
        console2.log("price target:", _formatE18(targetE18));
        if (plan.amountIn == 0) {
            console2.log("the pool is already at the target");
            return;
        }
        bool buysBase = plan.tokenOut == p.base;
        console2.log(string.concat("swap: ", buysBase ? "buy " : "sell ", base));
        console2.log(string.concat("  in:  ", _formatAmount(plan.tokenIn, plan.amountIn)));
        console2.log(string.concat("  out: ", _formatAmount(plan.tokenOut, plan.amountOut)));
        console2.log("price after (simulated):", _formatE18(_priceE18(p, plan.sqrtPriceAfter)));
    }

    /// @dev A raw amount in whole tokens with the symbol, for a token with at most 18 decimals.
    function _formatAmount(address token, uint256 amount) internal view returns (string memory) {
        return string.concat(_formatE18(amount * 10 ** (18 - _decimals(token))), " ", IERC20Metadata(token).symbol());
    }
}
