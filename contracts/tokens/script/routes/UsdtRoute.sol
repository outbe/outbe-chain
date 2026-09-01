// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {RouteSpec, BaseRoute, SyntheticSource} from "./BaseRoute.sol";
import {USDT} from "../../src/canonical/USDT.sol";
import {BridgeableERC20Stable} from "../../src/synthetic/BridgeableERC20Stable.sol";

/// @dev The USDT route: canonical USDT on the external chain, ERC-7802 synthetic on Outbe.
///      Everything specific to USDT lives here - the salt labels, which side is canonical, the token metadata and the
///      dev-mock bootstrap. Adding another token means adding a sibling of this file, not editing shared code.
abstract contract UsdtRoute is BaseRoute {
    /// @dev Labels are part of the CREATE3 address. Changing either string relocates every USDT deployment.
    function usdtSpec() public pure returns (RouteSpec memory) {
        return RouteSpec({
            tokenLabel: "USDT",
            canonicalOnOutbe: false,
            // Point this at the issuer's USDT on a real network; leave it unset on a testnet, where no real USDT
            // exists and a mintable mock is deployed instead.
            canonicalTokenEnv: "CANONICAL_USDT_TOKEN",
            factoryTokenEnv: "",
            syntheticSource: SyntheticSource.Erc7802
        });
    }

    function deployUsdt(address createX, string memory salt) public returns (address token, address tokenBridge) {
        RouteSpec memory spec = usdtSpec();
        bool canonical = _isCanonicalHere(spec);

        (token, tokenBridge) = _deployRoute(createX, salt, spec, _usdtInitCode(canonical));

        // Bootstrap the dev mock the first time it is deployed. Keyed on `totalSupply` rather than on "was just
        // deployed", so a re-run after a successful deploy does not mint a second time.
        if (canonical && USDT(token).totalSupply() == 0) {
            USDT(token).mint(_mintRecipient(), vm.envOr("INITIAL_MINT_AMOUNT", uint256(1_000_000_000e6)));
        }
    }

    /// @dev Not `view`: with `dynamic_test_linking` on, `type(T).creationCode` compiles to a state-modifying
    ///      `vm.getCode()` cheatcode call.
    function _usdtInitCode(bool canonical) internal returns (bytes memory) {
        return canonical
            ? type(USDT).creationCode
            : abi.encodePacked(
                type(BridgeableERC20Stable).creationCode, abi.encode("USDT", "USDT", uint8(6), uint16(840), _owner())
            );
    }
}
