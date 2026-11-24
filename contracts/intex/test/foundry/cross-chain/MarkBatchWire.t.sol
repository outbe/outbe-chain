// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";

/// @dev One day's series in one reference currency travel as a single mark message, and the target
///      applies every series it carries.
contract MarkBatchWireTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    uint32 internal constant EMISSION_DAY = 20250101;
    bytes14 internal constant USD_SERIES = "20250101-USD-U";
    bytes14 internal constant EUR_SERIES = "20250101-EUR-U";

    TargetRouter internal bnbRouter;
    IntexAuction internal auction;
    IntexNFT1155 internal intex;

    address internal admin = address(this);
    address internal outbeIca = makeAddr("outbeIca");

    function setUp() public {
        _setUpBridge();

        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);
        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        _wireOutbeIca(bnbRouter, outbeIca);

        bnbRouter.setDependencies(address(auction), address(intex), admin);
        intex.grantRole(intex.ROUTER_ROLE(), address(bnbRouter));
    }

    function _createBoth() internal {
        intex.createSeries(CreateSeriesLib.params(EMISSION_DAY, 10_000, 0));
        IIntexNFT1155.CreateSeriesParams memory eur = CreateSeriesLib.params(EMISSION_DAY, 10_000, 0);
        eur.seriesId = EUR_SERIES;
        intex.createSeries(eur);
    }

    function _state(bytes14 seriesId) internal view returns (uint8) {
        return uint8(intex.readData(seriesId).state);
    }

    function test_oneMessageCarriesTheWholeGroupToTheTarget() public {
        _createBoth();
        bytes14[] memory batch = MarkBatchLib.two(USD_SERIES, EUR_SERIES);

        _deliver(
            OUTBE_CHAIN_ID,
            outbeIca,
            address(bnbRouter),
            BridgeMsgCodec.encodeIntexSeriesCallNotice(EMISSION_DAY, uint32(block.timestamp), batch)
        );

        assertEq(_state(USD_SERIES), uint8(IIntexNFT1155.IntexState.Called), "USD series marked");
        assertEq(_state(EUR_SERIES), uint8(IIntexNFT1155.IntexState.Called), "EUR series marked");
    }

    /// @notice One series missing on the target must not cost the rest of its batch.
    function test_aMissingSeriesParksAloneWhileTheRestApply() public {
        intex.createSeries(CreateSeriesLib.params(EMISSION_DAY, 10_000, 0));

        _deliver(
            OUTBE_CHAIN_ID,
            outbeIca,
            address(bnbRouter),
            BridgeMsgCodec.encodeIntexSeriesCallNotice(
                EMISSION_DAY, uint32(block.timestamp), MarkBatchLib.two(USD_SERIES, EUR_SERIES)
            )
        );

        assertEq(_state(USD_SERIES), uint8(IIntexNFT1155.IntexState.Called), "the known series applied");
        assertEq(
            bnbRouter.failedIntexSeriesCallNoticeApply(EUR_SERIES),
            uint32(block.timestamp),
            "only the missing one waits"
        );
        assertEq(bnbRouter.failedIntexSeriesCallNoticeApply(USD_SERIES), 0, "the applied one has no slot");
    }
}
