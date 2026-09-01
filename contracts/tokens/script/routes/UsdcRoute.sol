// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {RouteSpec, BaseRoute, SyntheticSource} from "./BaseRoute.sol";
import {USDC} from "../../src/canonical/USDC.sol";

/// @dev The USDC route: canonical USDC on the external chain, factory-issued stablecoin on Outbe. The synthetic side
///      is never deployed here - the stablecoin is created by governance, so `FACTORY_USDC_TOKEN` must name it.
abstract contract UsdcRoute is BaseRoute {
    /// @dev Labels are part of the CREATE3 address. Changing this string relocates every USDC deployment.
    function usdcSpec() public pure returns (RouteSpec memory) {
        return RouteSpec({
            tokenLabel: "USDC",
            canonicalOnOutbe: false,
            // Point this at the issuer's USDC on a real network; leave it unset on a testnet, where no real USDC
            // exists and a mintable mock is deployed instead.
            canonicalTokenEnv: "CANONICAL_USDC_TOKEN",
            factoryTokenEnv: "FACTORY_USDC_TOKEN",
            syntheticSource: SyntheticSource.TokenFactory
        });
    }

    function deployUsdc(address createX, string memory salt) public returns (address token, address tokenBridge) {
        RouteSpec memory spec = usdcSpec();
        bool canonical = _isCanonicalHere(spec);

        (token, tokenBridge) = _deployRoute(createX, salt, spec, _usdcInitCode(canonical));

        if (canonical && USDC(token).totalSupply() == 0) {
            USDC(token).mint(_mintRecipient(), vm.envOr("INITIAL_MINT_AMOUNT", uint256(1_000_000_000e6)));
        }
    }

    /// @dev Empty on the synthetic side: the stablecoin is issued by governance, so there is nothing to place there.
    ///      `_deployRoute` rejects an unset `FACTORY_USDC_TOKEN` rather than deploying.
    ///      Not `view`: with `dynamic_test_linking` on, `type(T).creationCode` compiles to a state-modifying
    ///      `vm.getCode()` cheatcode call.
    function _usdcInitCode(bool canonical) internal returns (bytes memory) {
        return canonical ? type(USDC).creationCode : bytes("");
    }
}
