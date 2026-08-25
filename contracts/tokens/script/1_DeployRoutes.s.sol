// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {UsdtRoute} from "./routes/UsdtRoute.sol";
import {WcoenRoute} from "./routes/WcoenRoute.sol";

/// @dev Assembles the routes defined in `script/routes/` and deploys them on the connected chain. Which side of each
///      route is canonical is derived from `OUTBE_CHAIN_ID`, so the same command runs on every chain.
///
///      Adding a token is a new file in `script/routes/` plus two lines here — no shared code changes.
contract DeployRoutes is UsdtRoute, WcoenRoute {
    function run() public virtual {
        string memory salt = vm.envString("CONTRACT_SALT");
        address createX = vm.envAddress("CREATEX_ADDRESS");

        vm.startBroadcast(_pk());
        (address usdt, address usdtBridge) = deployUsdt(createX, salt);
        (address wcoen, address wcoenBridge) = deployWcoen(createX, salt);
        vm.stopBroadcast();

        _logRoute("USDT", usdt, usdtBridge);
        _logRoute("WCOEN", wcoen, wcoenBridge);
    }
}
