// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

// LOCAL TEST DRIVER v2 — not for commit. Fires OriginRouter sends on the LIVE July-30 deploy
// (multi-target main) to verify bridge delivery. ABI declared inline to match deployed main.
// Env: DEPLOYER_PRIVATE_KEY (= 0x2Af7 admin), SERIES (default 20260801 — a past day with no auction).

import {Script, console} from "forge-std/Script.sol";

interface IRouterV2 {
    struct AuctionStageStartParams {
        uint32 worldwideDay;
        uint32 commitEnd;
        uint32 revealEnd;
        uint32 issuanceEnd;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint128 promisLoadMinor;
        uint32 minIntexBidRate;
        uint64 entryPrice;
        uint64 floorPriceMinor;
        uint64 callPriceMinor;
        uint32 intexCallPeriod;
        uint16 callWindowDays;
        uint16 callThresholdDays;
        uint16 minIntexBidQuantity;
        uint128 commitBondMinor;
        uint8 dayState;
    }

    struct IssuanceInstructionsParams {
        uint32 dstChainId;
        uint32 seriesId;
        uint32 worldwideDay;
        uint32 issuedIntexCount;
        uint128 promisLoadMinor;
        uint64 entryPriceMinor;
        uint64 floorPriceMinor;
        uint32 intexCallPeriod;
        uint16 issuanceCurrency;
        uint16 referenceCurrency;
        uint16 callWindowDays;
        uint16 callThresholdDays;
        uint64 callPriceMinor;
        address[] recipients;
        uint256[] quantities;
    }

    function targets() external view returns (uint32[] memory);
    function targetsOf(uint32 worldwideDay) external view returns (uint32[] memory);
    function sendAuctionStageStart(AuctionStageStartParams calldata params) external payable;
    function sendAuctionStageClearing(uint32 worldwideDay) external payable;
    function sendAuctionResult(
        uint32 dstChainId,
        uint32 worldwideDay,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external payable returns (bytes32);
    function sendIssuanceInstructions(IssuanceInstructionsParams calldata params) external payable returns (bytes32);
    function sendMarkQualified(uint32 seriesId) external payable;
    function sendMarkCalled(uint32 seriesId) external payable;
}

interface IRolesV2 {
    function DESIS_ROLE() external view returns (bytes32);
    function INTEX_FACTORY_ROLE() external view returns (bytes32);
    function grantRole(bytes32 role, address account) external;
    function hasRole(bytes32 role, address account) external view returns (bool);
}

contract ManualDrive is Script {
    address constant ORIGIN_ROUTER = 0x67129C422bDC2c8984DbF381B6ec4515fE2BbD29;
    address constant USER = 0x78e5Bbd38feC73C39F0E798823EDEAD22552fdCf;

    uint16 constant CCY = 840;
    uint128 constant PROMIS_LOAD = 1e18;

    function _pk() internal view returns (uint256) {
        return vm.envUint("DEPLOYER_PRIVATE_KEY");
    }

    function _series() internal view returns (uint32) {
        return uint32(vm.envOr("SERIES", uint256(20260801)));
    }

    function setup() external {
        uint256 pk = _pk();
        address me = vm.addr(pk);
        IRolesV2 r = IRolesV2(ORIGIN_ROUTER);
        bytes32 desis = r.DESIS_ROLE();
        bytes32 factory = r.INTEX_FACTORY_ROLE();
        vm.startBroadcast(pk);
        if (!r.hasRole(desis, me)) r.grantRole(desis, me);
        if (!r.hasRole(factory, me)) r.grantRole(factory, me);
        vm.stopBroadcast();
        console.log("roles granted to", me);
        uint32[] memory t = IRouterV2(ORIGIN_ROUTER).targets();
        for (uint256 i = 0; i < t.length; i++) {
            console.log("registered target chainId:", t[i]);
        }
    }

    function stageStart() external {
        IRouterV2.AuctionStageStartParams memory p = IRouterV2.AuctionStageStartParams({
            worldwideDay: _series(),
            commitEnd: uint32(block.timestamp + 90),
            revealEnd: uint32(block.timestamp + 1 hours),
            issuanceEnd: uint32(block.timestamp + 3 days),
            issuanceCurrency: CCY,
            referenceCurrency: CCY,
            promisLoadMinor: PROMIS_LOAD,
            minIntexBidRate: 1,
            entryPrice: 1_000_000,
            floorPriceMinor: 900_000,
            callPriceMinor: 1_100_000,
            intexCallPeriod: 7 days,
            callWindowDays: 1,
            callThresholdDays: 1,
            minIntexBidQuantity: 1,
            commitBondMinor: 0,
            dayState: 1 // Green
        });
        vm.startBroadcast(_pk());
        IRouterV2(ORIGIN_ROUTER).sendAuctionStageStart(p);
        vm.stopBroadcast();
        console.log("stageStart sent for", _series());
    }

    function stageClearing() external {
        vm.startBroadcast(_pk());
        IRouterV2(ORIGIN_ROUTER).sendAuctionStageClearing(_series());
        vm.stopBroadcast();
        console.log("stageClearing sent for", _series());
    }

    // Create the NFT series on both targets: issuance with empty recipients (valid per contract docs).
    function issuanceBoth() external {
        uint32 day = _series();
        address[] memory recips = new address[](1);
        recips[0] = USER;
        uint256[] memory qtys = new uint256[](1);
        qtys[0] = 1;
        uint32[2] memory dsts = [uint32(97), uint32(54322345)];
        vm.startBroadcast(_pk());
        for (uint256 i = 0; i < 2; i++) {
            IRouterV2(ORIGIN_ROUTER).sendIssuanceInstructions(
                IRouterV2.IssuanceInstructionsParams({
                    dstChainId: dsts[i],
                    seriesId: day,
                    worldwideDay: day,
                    issuedIntexCount: 1,
                    promisLoadMinor: PROMIS_LOAD,
                    entryPriceMinor: 1_000_000,
                    floorPriceMinor: 900_000,
                    intexCallPeriod: 7 days,
                    issuanceCurrency: CCY,
                    referenceCurrency: CCY,
                    callWindowDays: 1,
                    callThresholdDays: 1,
                    callPriceMinor: 1_100_000,
                    recipients: recips,
                    quantities: qtys
                })
            );
        }
        vm.stopBroadcast();
        console.log("issuance (empty) sent to 97 and 54322345 for", day);
    }

    function markQualified() external {
        vm.startBroadcast(_pk());
        IRouterV2(ORIGIN_ROUTER).sendMarkQualified(_series());
        vm.stopBroadcast();
    }
}
