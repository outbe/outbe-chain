/**
 * OriginMessenger Hardhat Tests
 *
 * Note: Full cross-chain tests are in test/foundry/cross-chain/OriginMessenger.t.sol (Foundry)
 * These tests cover the BNB-side contract functions that OriginMessenger drives via LayerZero.
 *
 * OriginMessenger sends:
 * - AUCTION_STAGE (start, reveal, clearing) -> IntexAuction.auctionStart, startRevealingBidsStage, startClearingStage
 * - AUCTION_RESULT -> IntexAuction.executeAuctionClearing
 * - ISSUANCE_INSTRUCTIONS -> IntexNFT1155.createSeries + mintBatch
 * - REFUND_INSTRUCTIONS -> EscrowAdapter.finalizeAuction
 * - MARK_CALLED -> IntexNFT1155.markCalled
 *
 * All auction/series messages are keyed by `seriesId` (uint32).
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { network } from "hardhat";
import { deployIntexNFT1155, deployIntexAuction } from "../_helpers.js";

describe("OriginMessenger", async function () {
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

  // Forced-call trigger params (IntexNFT1155.IntexCallTrigger).
  const CALL_TRIGGER = {
    windowDays: 30,
    thresholdDays: 5,
    callPriceMinor: 25n * 10n ** 6n,
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

  // --- IntexAuction contract tests (what BNB receives from Outbe) ---

  it("Should process auction result via executeAuctionClearing (AUCTION_RESULT handler)", async function () {
    const auction = await deployIntexAuction(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await auction.write.auctionStart(
      [
        SERIES_ID,
        { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
        {
          promisLoadMinor: PROMIS_LOAD_MINOR,
          minIntexBidPrice: MIN_BID_PRICE,
          costAmountMinor: COST_AMOUNT_MINOR,
          floorPriceMinor: FLOOR_PRICE_MINOR,
          minIntexBidQuantity: MIN_BID_QUANTITY,
        },
      ],
      { account: bridge.account.address }
    );

    // Green-day signal; stages then become time-derived.
    await auction.write.startRevealingBidsStage([SERIES_ID, true], {
      account: bridge.account.address,
    });

    // Advance past revealEnd so the auction is in the Issuance stage.
    await networkHelpers.time.increaseTo(schedule.revealEnd + 1);

    await auction.write.startClearingStage([SERIES_ID], {
      account: bridge.account.address,
    });

    // No bids are revealed in this handler-wiring test, so the clearing result has no winners
    // (wonBidsCount must not exceed the on-chain revealed-bid count).
    await auction.write.executeAuctionClearing(
      [SERIES_ID, 0, 75n * 10n ** 6n, 0],
      { account: bridge.account.address }
    );

    const auctionData = await auction.read.getAuctionInfo([SERIES_ID]);
    assert.equal(auctionData.result.issuedIntexCount, 0);
    assert.equal(auctionData.result.auctionIntexClearingPrice, 75n * 10n ** 6n);
    assert.equal(auctionData.result.wonBidsCount, 0);
  });

  // --- IntexNFT1155 contract tests (what BNB receives from Outbe) ---

  it("Should create series via createSeries (ISSUANCE_INSTRUCTIONS handler)", async function () {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    await intex.write.createSeries([SERIES_ID, AMOUNT, 0], { account: bridge.account.address });

    const seriesData = await intex.read.readData([SERIES_ID]);
    assert.equal(seriesData.issuedIntexCount, AMOUNT);
  });

  it("Should mint batch via mintBatch (ISSUANCE_INSTRUCTIONS handler)", async function () {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    await intex.write.createSeries([SERIES_ID, AMOUNT, 0], { account: bridge.account.address });

    await intex.write.mintBatch(
      [[user1.account.address], [AMOUNT], SERIES_ID],
      { account: bridge.account.address }
    );

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
  });

  // --- Access Control tests ---

  it("Should only allow RELAYER_ROLE to call executeAuctionClearing", async function () {
    const auction = await deployIntexAuction(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await auction.write.auctionStart(
      [
        SERIES_ID,
        { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
        {
          promisLoadMinor: PROMIS_LOAD_MINOR,
          minIntexBidPrice: MIN_BID_PRICE,
          costAmountMinor: COST_AMOUNT_MINOR,
          floorPriceMinor: FLOOR_PRICE_MINOR,
          minIntexBidQuantity: MIN_BID_QUANTITY,
        },
      ],
      { account: bridge.account.address }
    );

    await auction.write.startRevealingBidsStage([SERIES_ID, true], {
      account: bridge.account.address,
    });

    await networkHelpers.time.increaseTo(schedule.revealEnd + 1);

    await auction.write.startClearingStage([SERIES_ID], {
      account: bridge.account.address,
    });

    await viem.assertions.revertWithCustomError(
      auction.write.executeAuctionClearing(
        [SERIES_ID, 500, 75n * 10n ** 6n, 42],
        { account: user1.account.address }
      ),
      auction,
      "AccessControlUnauthorizedAccount"
    );
  });

  it("Should only allow RELAYER_ROLE to call mintBatch", async function () {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    await intex.write.createSeries([SERIES_ID, AMOUNT, 0], { account: bridge.account.address });

    await viem.assertions.revertWithCustomError(
      intex.write.mintBatch(
        [[user1.account.address], [AMOUNT], SERIES_ID],
        { account: user1.account.address }
      ),
      intex,
      "AccessControlUnauthorizedAccount"
    );
  });

  it("Should only allow RELAYER_ROLE to call startRevealingBidsStage", async function () {
    const auction = await deployIntexAuction(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await auction.write.auctionStart(
      [
        SERIES_ID,
        { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
        {
          promisLoadMinor: PROMIS_LOAD_MINOR,
          minIntexBidPrice: MIN_BID_PRICE,
          costAmountMinor: COST_AMOUNT_MINOR,
          floorPriceMinor: FLOOR_PRICE_MINOR,
          minIntexBidQuantity: MIN_BID_QUANTITY,
        },
      ],
      { account: bridge.account.address }
    );

    await viem.assertions.revertWithCustomError(
      auction.write.startRevealingBidsStage([SERIES_ID, true], {
        account: user1.account.address,
      }),
      auction,
      "AccessControlUnauthorizedAccount"
    );
  });

  it("Should only allow RELAYER_ROLE to call startClearingStage", async function () {
    const auction = await deployIntexAuction(viem, [
      deployer.account.address,
      bridge.account.address,
    ]);

    const schedule = await futureSchedule();
    await auction.write.auctionStart(
      [
        SERIES_ID,
        { commitEnd: schedule.commitEnd, revealEnd: schedule.revealEnd, issuanceEnd: schedule.issuanceEnd },
        {
          promisLoadMinor: PROMIS_LOAD_MINOR,
          minIntexBidPrice: MIN_BID_PRICE,
          costAmountMinor: COST_AMOUNT_MINOR,
          floorPriceMinor: FLOOR_PRICE_MINOR,
          minIntexBidQuantity: MIN_BID_QUANTITY,
        },
      ],
      { account: bridge.account.address }
    );

    await auction.write.startRevealingBidsStage([SERIES_ID, true], {
      account: bridge.account.address,
    });

    await networkHelpers.time.increaseTo(schedule.commitEnd + 1);

    await viem.assertions.revertWithCustomError(
      auction.write.startClearingStage([SERIES_ID], {
        account: user1.account.address,
      }),
      auction,
      "AccessControlUnauthorizedAccount"
    );
  });
});

/**
 * Note: Full cross-chain integration tests for OriginMessenger are in Foundry:
 * test/foundry/cross-chain/OriginMessenger.t.sol
 *
 * Those tests use LayerZero's TestHelperOz5 to simulate cross-chain messaging
 * with EndpointV2Mock contracts.
 */
