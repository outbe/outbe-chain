// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";

/// @notice Drive auction proceeds into the origin router without running an auction.
///
/// @dev The proceeds path guards itself three ways: only the registered token
///      bridge may deliver, the source chain must be in the day's frozen target
///      snapshot, and the sender must equal the registered peer. The snapshot is
///      frozen only by `sendAuctionStageStart`, so this script starts a day under
///      DESIS_ROLE rather than reproducing the whole metadosis path. Roles the
///      precompiles hold in production are taken by the deployer here; that is
///      the one concession, and it is why this lives in a script rather than in
///      the wiring tasks.
contract DriveProceeds is Script {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        address payable router = payable(vm.envAddress("ORIGIN_ROUTER"));
        address wcoen = vm.envAddress("WCOEN");
        address peer = vm.envAddress("TARGET_ROUTER");
        uint32 day = uint32(vm.envUint("WORLDWIDE_DAY"));
        uint256 amount = vm.envUint("PROCEEDS_AMOUNT");
        uint32 chainId = uint32(block.chainid);

        vm.startBroadcast(pk);

        OriginRouter(router).grantRole(OriginRouter(router).DESIS_ROLE(), deployer);

        // Only the fields the proceeds guards read matter; the rest carry
        // plausible values so the stage message encodes.
        uint32 now32 = uint32(block.timestamp);
        OriginRouter(router).sendAuctionStageStart(
            IOriginRouter.AuctionStageStartParams({
                worldwideDay: day,
                commitEnd: now32 + 1 hours,
                revealEnd: now32 + 2 hours,
                issuanceEnd: now32 + 3 hours,
                issuanceCurrency: 840,
                referenceCurrency: 840,
                promisLoadMinor: 1e18,
                minIntexBidRate: 1,
                entryPrice: 1e6,
                floorPriceMinor: 1e6,
                callPriceMinor: 2e6,
                callNoticePeriod: 0,
                callWindow: 1 days,
                callThreshold: 1 hours,
                minIntexBidQuantity: 1,
                commitBondMinor: 0,
                dayState: 1
            })
        );

        // The stub pays native out of its own balance when the router unwraps.
        (bool funded,) = payable(wcoen).call{value: amount}("");
        require(funded, "fund wcoen stub");

        OriginRouter(router).onCrosschainTokensReceived(
            chainId, InteroperableAddress.formatEvmV1(chainId, peer), amount, abi.encode(day)
        );

        vm.stopBroadcast();

        console.log("ProceedsDelivered day:", day);
        console.log("ProceedsDelivered amount:", amount);
    }
}
