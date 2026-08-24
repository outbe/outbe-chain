// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {RouteSpec, BaseRoute} from "./BaseRoute.sol";
import {WCOEN} from "../../src/native/WCOEN.sol";
import {BridgeableERC20} from "../../src/synthetic/BridgeableERC20.sol";

/// @dev The WCOEN route: canonical WCOEN on Outbe, ERC-7802 synthetic on the external chain — the mirror image of
///      the USDT route. Everything specific to WCOEN lives here.
abstract contract WcoenRoute is BaseRoute {
    /// @dev Labels are part of the CREATE3 address. Changing either string relocates every WCOEN deployment.
    function wcoenSpec() public pure returns (RouteSpec memory) {
        return RouteSpec({
            tokenLabel: "WCOEN",
            bridgeLabel: "WCOENBridge",
            canonicalOnOutbe: true,
            // Set only if a canonical WCOEN was already deployed on Outbe outside this script.
            canonicalTokenEnv: "CANONICAL_WCOEN_TOKEN"
        });
    }

    function deployWcoen(address createX, string memory salt) public returns (address token, address tokenBridge) {
        RouteSpec memory spec = wcoenSpec();
        return _deployRoute(createX, salt, spec, _wcoenInitCode(_isCanonicalHere(spec)));
    }

    /// @dev Not `view`: with `dynamic_test_linking` on, `type(T).creationCode` compiles to a state-modifying
    ///      `vm.getCode()` cheatcode call.
    function _wcoenInitCode(bool canonical) internal returns (bytes memory) {
        return canonical
            ? type(WCOEN).creationCode
            : abi.encodePacked(
                type(BridgeableERC20).creationCode, abi.encode("Wrapped COEN", "WCOEN", uint8(6), _owner())
            );
    }
}
