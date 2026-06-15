/**
 * TargetMessenger Hardhat Tests
 *
 * Note: Full cross-chain tests are in test/foundry/cross-chain/TargetMessenger.t.sol (Foundry)
 * These tests cover the BNB-side contract functions that TargetMessenger drives on receipt
 * of LayerZero messages from Outbe.
 *
 * All auction/series messages are keyed by `seriesId` (uint32).
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { network } from "hardhat";
import { deployIntexNFT1155 } from "../_helpers.js";

describe("TargetMessenger", async function () {
  const { viem, networkHelpers } = await network.connect();

  const [deployer, bridge, user1] = await viem.getWalletClients();

  const SERIES_ID = 20250115; // yyyymmdd format; doubles as the IntexNFT1155 token id
  const TOKEN_ID = BigInt(SERIES_ID);
  const AMOUNT = 100n;
  const PROMIS_LOAD_MINOR = 1000n;
  const COST_AMOUNT_MINOR = 100n * 10n ** 6n;
  const FLOOR_PRICE_MINOR = 40n * 10n ** 6n;
  const MIN_BID_PRICE = 50n * 10n ** 6n;
  const SETTLEMENT_TOKEN_ALIAS = 840; // ISO 4217 USD
  const MIN_BID_QUANTITY = 1;
  const DEFAULT_CALL_PERIOD = 21 * 24 * 60 * 60; // 21 days, applied when createSeries gets 0

  // Forced-call trigger params (IntexNFT1155.IntexCallTrigger).
  const CALL_TRIGGER = {
    windowDays: 30,
    thresholdDays: 5,
    coenPriceCallTrigger: 25n * 10n ** 6n,
  };

  // Build a strictly-increasing future schedule relative to the current block time.
  async function futureSchedule() {
    const now = await networkHelpers.time.latest();
    return {
      commitEnd: now + 3600,
      revealEnd: now + 5400,
      issuanceEnd: now + 7200,
    };
  }

  function auctionParams() {
    return {
      promisLoadMinor: PROMIS_LOAD_MINOR,
      minIntexBidPrice: MIN_BID_PRICE,
      costAmountMinor: COST_AMOUNT_MINOR,
      floorPriceMinor: FLOOR_PRICE_MINOR,
      minIntexBidQuantity: MIN_BID_QUANTITY,
    };
  }

  // --- IntexAuction contract tests (what TargetMessenger._handleAuctionStage* calls) ---

  it("Should start auction via auctionStart (AUCTION_STAGE_START handler)", async function () {
    const auction = await viem.deployContract("IntexAuction", [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await auction.write.auctionStart(
      [
        SERIES_ID,
        { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
        auctionParams(),
      ],
      { account: bridge.account.address }
    );

    const auctionData = await auction.read.getAuctionInfo([SERIES_ID]);
    assert.equal(auctionData.worldwideDayState, 0); // Unknown (awaiting bridge signal)
    assert.equal(auctionData.schedule.commitEnd, schedule.commitEnd);
    assert.equal(auctionData.params.promisLoadMinor, PROMIS_LOAD_MINOR);
    // Fresh auction sits in the CommittingBids stage.
    assert.equal(await auction.read.getAuctionStage([SERIES_ID]), 0);
  });

  it("Should start reveal stage via startRevealingBidsStage (AUCTION_STAGE_REVEAL handler)", async function () {
    const auction = await viem.deployContract("IntexAuction", [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await auction.write.auctionStart(
      [
        SERIES_ID,
        { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
        auctionParams(),
      ],
      { account: bridge.account.address }
    );

    await auction.write.startRevealingBidsStage([SERIES_ID, true], {
      account: bridge.account.address,
    });

    const auctionData = await auction.read.getAuctionInfo([SERIES_ID]);
    assert.equal(auctionData.worldwideDayState, 1); // Green (0=Unknown, 1=Green, 2=Red)
  });

  it("Should start clearing stage via startClearingStage (AUCTION_STAGE_CLEARING handler)", async function () {
    const auction = await viem.deployContract("IntexAuction", [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await auction.write.auctionStart(
      [
        SERIES_ID,
        { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
        auctionParams(),
      ],
      { account: bridge.account.address }
    );

    await auction.write.startRevealingBidsStage([SERIES_ID, true], {
      account: bridge.account.address,
    });

    // startClearingStage requires the RevealingBids (or Issuance) stage; advance past commitEnd.
    await networkHelpers.time.increaseTo(schedule.commitEnd + 1);

    await auction.write.startClearingStage([SERIES_ID], {
      account: bridge.account.address,
    });

    // Stages are time-derived; once past revealEnd the auction is in Issuance.
    await networkHelpers.time.increaseTo(schedule.revealEnd + 1);
    assert.equal(await auction.read.getAuctionStage([SERIES_ID]), 2); // Issuance
  });

  // --- IntexNFT1155 contract tests (what TargetMessenger._handleIssuanceInstructions calls) ---

  it("Should create series and mint via createSeries + mint (ISSUANCE_INSTRUCTIONS handler)", async function () {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    await intex.write.createSeries([SERIES_ID, AMOUNT, 0], { account: bridge.account.address });

    await intex.write.mint([user1.account.address, AMOUNT, SERIES_ID], {
      account: bridge.account.address,
    });

    assert.equal(
      await intex.read.balanceOf([user1.account.address, TOKEN_ID]),
      AMOUNT
    );
  });

  it("Should mark as called via markCalled (MARK_CALLED handler)", async function () {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    await intex.write.createSeries([SERIES_ID, AMOUNT, 0], { account: bridge.account.address });

    await intex.write.markCalled([SERIES_ID], {
      account: bridge.account.address,
    });

    const seriesData = await intex.read.readData([SERIES_ID]);
    assert.equal(seriesData.state, 2); // Called state (0=Issued, 1=Qualified, 2=Called)
    assert.notEqual(seriesData.calledAt, 0);
    assert.equal(seriesData.intexCallPeriod, DEFAULT_CALL_PERIOD); // default 21 days
  });

  // --- Access Control tests ---

  it("Should only allow RELAYER_ROLE to call auctionStart", async function () {
    const auction = await viem.deployContract("IntexAuction", [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await viem.assertions.revertWithCustomError(
      auction.write.auctionStart(
        [
          SERIES_ID,
          { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
          auctionParams(),
        ],
        { account: user1.account.address }
      ),
      auction,
      "AccessControlUnauthorizedAccount"
    );
  });

  it("Should only allow RELAYER_ROLE to call createSeries", async function () {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    await viem.assertions.revertWithCustomError(
      intex.write.createSeries([SERIES_ID, AMOUNT, 0], { account: user1.account.address }),
      intex,
      "AccessControlUnauthorizedAccount"
    );
  });

  it("Should only allow RELAYER_ROLE to call markCalled", async function () {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    await intex.write.createSeries([SERIES_ID, AMOUNT, 0], { account: bridge.account.address });

    await viem.assertions.revertWithCustomError(
      intex.write.markCalled([SERIES_ID], { account: user1.account.address }),
      intex,
      "AccessControlUnauthorizedAccount"
    );
  });
});

/**
 * Note: Full cross-chain integration tests for TargetMessenger are in Foundry:
 * test/foundry/cross-chain/TargetMessenger.t.sol
 *
 * Those tests use LayerZero's TestHelperOz5 to simulate cross-chain messaging
 * with EndpointV2Mock contracts.
 */
