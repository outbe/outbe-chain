// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {VwapRegistry} from "@contracts/target/VwapRegistry.sol";

/// @title VwapSourceWiring
/// @author Outbe
/// @notice The daily VWAP source of a chain's IntexNFT1155: the IntexFactory precompile on the origin, a
///         VwapRegistry the TargetRouter records into everywhere else.
abstract contract VwapSourceWiring is BaseScript {
    address internal constant INTEX_FACTORY = address(0x1015);

    function deployVwapRegistry(Create3Factory factory, address deployer, address router) internal returns (address) {
        return deployProxy(
            factory,
            deployer,
            "VwapRegistry",
            address(new VwapRegistry()),
            abi.encodeCall(VwapRegistry.initialize, (deployer, router))
        );
    }

    function wireVwapSource(address nft, address router, address source) internal {
        if (source != INTEX_FACTORY && address(TargetRouter(payable(router)).vwapRegistry()) != source) {
            TargetRouter(payable(router)).setVwapRegistry(source);
        }
        if (IntexNFT1155(nft).vwapSource() != source) IntexNFT1155(nft).setVwapSource(source);
    }
}
