// We don't have Ethereum specific assertions in Hardhat 3 yet
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { network } from "hardhat";
import { deployIntexNFT1155 } from "./_helpers.js";

describe("IntexNFT1155", async function () {
  const { viem } = await network.connect();
  const publicClient = await viem.getPublicClient();
  const testClient = await viem.getTestClient();

  // Test accounts - using hardhat's default accounts
  const [deployer, _, __, bridger, user1, user2] = await viem.getWalletClients();

  // Series are keyed by uint32 seriesId (yyyymmdd convention).
  const SERIES_ID_1 = 20250101;
  const SERIES_ID_2 = 20250202;
  const SERIES_ID_3 = 20250303;

  // Issued token id == uint256(seriesId).
  const TOKEN_ID_1 = BigInt(SERIES_ID_1);
  const TOKEN_ID_2 = BigInt(SERIES_ID_2);
  const TOKEN_ID_3 = BigInt(SERIES_ID_3);

  // createSeries parameters reused across tests.
  const PROMIS_LOAD_MINOR = 100n * 10n ** 18n;
  const STRIKE_PRICE = 1500000n; // 1.50 with 6 decimals
  const PRICE_FLOOR = 1000000n; // 1.00 with 6 decimals
  const SETTLEMENT_TOKEN_ALIAS = 840; // USD
  const DEFAULT_CALL_PERIOD = 21 * 24 * 60 * 60;
  // Sized well above any per-mint quantity in this suite so existing tests exercise
  // lifecycle behavior independently of the supply cap.
  const ISSUED_INTEX_COUNT = 10_000;
  // Sentinel page size that always drains the holder set in one expireSeries call.
  const EXPIRE_FULL_LIMIT = 2n ** 256n - 1n;

  // IntexCallTrigger{uint16 windowDays, uint16 thresholdDays, uint64 callPriceMinor}
  const TRIGGER = { windowDays: 30, thresholdDays: 5, callPriceMinor: 2000000n };

  /**
   * createSeries args in the new ABI order:
   * (seriesId, issuedIntexCount, promisLoadMinor, costAmountMinor, floorPriceMinor,
   *  intexCallPeriod, settlementTokenAlias, trigger)
   */
  function seriesArgs(
    seriesId: number,
    overrides: {
      issuedIntexCount?: number;
      promisLoadMinor?: bigint;
      strikePrice?: bigint;
      priceFloor?: bigint;
      callPeriod?: bigint;
      settlementTokenAlias?: number;
      trigger?: typeof TRIGGER;
    } = {},
  ) {
    return [
      seriesId,
      overrides.issuedIntexCount ?? ISSUED_INTEX_COUNT,
      overrides.promisLoadMinor ?? PROMIS_LOAD_MINOR,
      overrides.strikePrice ?? STRIKE_PRICE,
      overrides.priceFloor ?? PRICE_FLOOR,
      overrides.callPeriod ?? 0n,
      overrides.settlementTokenAlias ?? SETTLEMENT_TOKEN_ALIAS,
      overrides.trigger ?? TRIGGER,
    ] as const;
  }

  it("Should deploy with correct initial state", async function () {
    const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

    // Check initial roles
    assert(await nft.read.hasRole([await nft.read.DEFAULT_ADMIN_ROLE(), deployer.account.address]));
    assert(await nft.read.hasRole([await nft.read.RELAYER_ROLE(), bridger.account.address]));
  });

  describe("Intex NFT1155 Functions", async function () {
    it("Should allow bridge to create series", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      const data = await nft.read.readData([SERIES_ID_1]);
      assert.equal(data.state, 0); // Issued
      assert.equal(data.status, 0); // Issued
      assert.equal(data.promisLoadMinor, PROMIS_LOAD_MINOR);
      assert.equal(data.costAmountMinor, STRIKE_PRICE);
      assert.equal(data.floorPriceMinor, PRICE_FLOOR);
      assert.equal(data.settlementTokenAlias, SETTLEMENT_TOKEN_ALIAS);
      assert(data.issuedAt > 0);
      assert.equal(data.calledAt, 0);
      assert.equal(data.intexCallPeriod, DEFAULT_CALL_PERIOD); // default 21 days
      assert.equal(data.intexCallTrigger.windowDays, TRIGGER.windowDays);
      assert.equal(data.intexCallTrigger.thresholdDays, TRIGGER.thresholdDays);
      assert.equal(data.intexCallTrigger.callPriceMinor, TRIGGER.callPriceMinor);
    });

    it("Should prevent non-bridge from creating series", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await viem.assertions.revertWithCustomError(
        nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should revert when creating duplicate series", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address }),
        nft,
        "TokenAlreadyExists",
      );
    });

    it("Should allow bridge to mint tokens", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const quantity = 10n;

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, quantity, SERIES_ID_1], { account: bridger.account.address });

      // Check token balance
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), quantity);
    });

    it("Should prevent non-bridge from minting", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should revert when minting to zero address", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.mint(["0x0000000000000000000000000000000000000000", 10n, SERIES_ID_1], { account: bridger.account.address }),
        nft,
        "ZeroAddress",
      );
    });

    it("Should revert when minting to non-existent series", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await viem.assertions.revertWithCustomError(
        nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address }),
        nft,
        "NonexistentToken",
      );
    });

    it("Should revert when minting a quantity above uint16 max", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.mint([user1.account.address, 65536n, SERIES_ID_1], { account: bridger.account.address }),
        nft,
        "QuantityTooLarge",
      );
    });

    it("Should allow bridge to mint batch", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const recipients = [user1.account.address, user2.account.address];
      const quantities = [5n, 10n];

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mintBatch([recipients, quantities, SERIES_ID_1], { account: bridger.account.address });

      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 5n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 10n);
    });

    it("Should record auction won count on single mint", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      // Auction won count should be recorded
      assert.equal(await nft.read.getAuctionWonCount([SERIES_ID_1, user1.account.address]), 10);
      // Non-minted address should return 0
      assert.equal(await nft.read.getAuctionWonCount([SERIES_ID_1, user2.account.address]), 0);
    });

    it("Should record auction won count on batch mint", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const recipients = [user1.account.address, user2.account.address];
      const quantities = [5n, 10n];

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mintBatch([recipients, quantities, SERIES_ID_1], { account: bridger.account.address });

      // Auction won counts should be recorded for each recipient
      assert.equal(await nft.read.getAuctionWonCount([SERIES_ID_1, user1.account.address]), 5);
      assert.equal(await nft.read.getAuctionWonCount([SERIES_ID_1, user2.account.address]), 10);
    });

    it("Should preserve auction won count after transfer", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      // Transfer some tokens to user2
      await nft.write.safeTransferFrom([user1.account.address, user2.account.address, TOKEN_ID_1, 3n, "0x"], { account: user1.account.address });

      // Auction won count should remain unchanged for user1
      assert.equal(await nft.read.getAuctionWonCount([SERIES_ID_1, user1.account.address]), 10);
      // user2 received via transfer, not mint - should be 0
      assert.equal(await nft.read.getAuctionWonCount([SERIES_ID_1, user2.account.address]), 0);

      // Current balances are different from initial
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 7n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 3n);
    });

    it("Should revert when mintBatch array lengths mismatch", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const recipients = [user1.account.address, user2.account.address];
      const quantities = [5n];

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.mintBatch([recipients, quantities, SERIES_ID_1], { account: bridger.account.address }),
        nft,
        "ArrayLengthMismatch",
      );
    });

    it("Should allow bridge to mark a series as qualified", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      const data = await nft.read.readData([SERIES_ID_1]);
      assert.equal(data.state, 1); // Qualified
    });

    it("Should allow bridge to mark tokens as called", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      const data = await nft.read.readData([SERIES_ID_1]);
      assert.equal(data.state, 2); // Called (Issued=0, Qualified=1, Called=2)
      assert.notEqual(data.calledAt, 0);
      assert.equal(data.intexCallPeriod, DEFAULT_CALL_PERIOD); // default 21 days
    });

    it("Should prevent non-bridge from marking tokens as called", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.markCalled([SERIES_ID_1], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should revert when marking non-existent token as called", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await viem.assertions.revertWithCustomError(
        nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address }),
        nft,
        "NonexistentToken",
      );
    });

    it("Should revert when creating series with call period above the max", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);
      const tooLarge = 366n * 24n * 60n * 60n; // > 365 days

      await viem.assertions.revertWithCustomError(
        nft.write.createSeries(seriesArgs(SERIES_ID_1, { callPeriod: tooLarge }), { account: bridger.account.address }),
        nft,
        "InvalidCallPeriod",
      );
    });

    it("Should allow bridge to update call period", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const newCallPeriod = 30n * 24n * 60n * 60n; // 30 days

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });
      await nft.write.updateCallPeriod([SERIES_ID_1, newCallPeriod], { account: bridger.account.address });

      const data = await nft.read.readData([SERIES_ID_1]);
      assert.equal(BigInt(data.intexCallPeriod), newCallPeriod);
    });

    it("Should prevent non-bridge from updating call period", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const newCallPeriod = 30n * 24n * 60n * 60n;

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.updateCallPeriod([SERIES_ID_1, newCallPeriod], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should allow bridge to crosschainBurn tokens in Qualified state", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const quantity = 10n;
      const burnAmount = 5n;

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, quantity, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, burnAmount], { account: bridger.account.address });

      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), quantity - burnAmount);
    });

    it("Should reject bridge crosschainBurn in Issued state and require SYSTEM_RELAYER_ROLE in Called state", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const quantity = 10n;

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, quantity, SERIES_ID_1], { account: bridger.account.address });

      // CrosschainBurn in Issued: rejected.
      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 1n], { account: bridger.account.address }),
        nft,
        "BridgeStateForbidden",
      );

      // Qualified: allowed.
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 3n], { account: bridger.account.address });
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 7n);

      // Called for plain RELAYER: rejected. SYSTEM_RELAYER_ROLE allows it.
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });
      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 1n], { account: bridger.account.address }),
        nft,
        "BridgeStateForbidden",
      );

      await nft.write.grantRole([await nft.read.SYSTEM_RELAYER_ROLE(), bridger.account.address], {
        account: deployer.account.address,
      });
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 2n], { account: bridger.account.address });
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 5n);
    });

    it("Should reject bridge crosschainBurn on a Called series after the settlement deadline", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1, { callPeriod: 86400n }), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.grantRole([await nft.read.SYSTEM_RELAYER_ROLE(), bridger.account.address], {
        account: deployer.account.address,
      });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      const data0 = await nft.read.readData([SERIES_ID_1]);
      const derivedDeadline = BigInt(data0.calledAt) + BigInt(data0.intexCallPeriod);
      await testClient.setNextBlockTimestamp({ timestamp: derivedDeadline + 1n });
      await testClient.mine({ blocks: 1 });

      // Past the deadline even the system relayer is frozen out.
      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 1n], { account: bridger.account.address }),
        nft,
        "BridgeAfterDeadline",
      );
    });

    it("Should reject bridge crosschainMint on a Called series after the settlement deadline", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1, { callPeriod: 86400n }), { account: bridger.account.address });
      await nft.write.grantRole([await nft.read.SYSTEM_RELAYER_ROLE(), bridger.account.address], {
        account: deployer.account.address,
      });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      const data0 = await nft.read.readData([SERIES_ID_1]);
      const derivedDeadline = BigInt(data0.calledAt) + BigInt(data0.intexCallPeriod);
      await testClient.setNextBlockTimestamp({ timestamp: derivedDeadline + 1n });
      await testClient.mine({ blocks: 1 });

      // Mirror of crosschainBurn: no bridge-in past the window — crosschainMint cannot re-inflate supply.
      await viem.assertions.revertWithCustomError(
        nft.write.crosschainMint([user1.account.address, TOKEN_ID_1, 10n], { account: bridger.account.address }),
        nft,
        "BridgeAfterDeadline",
      );
    });

    it("Should prevent non-bridge from crosschainBurning tokens", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should revert when crosschainBurning non-existent token", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: bridger.account.address }),
        nft,
        "NonexistentToken",
      );
    });

    it("Should allow reading token data", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      const data = await nft.read.readData([SERIES_ID_1]);
      assert.equal(data.state, 0); // Issued
      assert.equal(data.promisLoadMinor, PROMIS_LOAD_MINOR);
      assert.equal(data.costAmountMinor, STRIKE_PRICE);
    });

    it("Should revert when reading non-existent token data", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await viem.assertions.revertWithCustomError(
        nft.read.readData([SERIES_ID_1]),
        nft,
        "NonexistentToken",
      );
    });
  });

  describe("Transfer Restrictions", async function () {
    it("Should allow transfers in Issued state", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      // Token should be transferable in Issued state
      await nft.write.safeTransferFrom([user1.account.address, user2.account.address, TOKEN_ID_1, 5n, "0x"], { account: user1.account.address });
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 5n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 5n);
    });

    it("Should allow transfers in Qualified state", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      // Issued tokens stay transferable in Qualified state.
      await nft.write.safeTransferFrom([user1.account.address, user2.account.address, TOKEN_ID_1, 5n, "0x"], { account: user1.account.address });
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 5n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 5n);
    });

    it("Should allow transfers in Called state (Issued tokens are transferable in every series state)", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      // Issued tokens stay transferable in Called state — only bridge is gated by series state.
      await nft.write.safeTransferFrom([user1.account.address, user2.account.address, TOKEN_ID_1, 5n, "0x"], { account: user1.account.address });
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 5n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 5n);
    });

    it("Should allow batch transfers when any token is in Called state", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { promisLoadMinor: 200n * 10n ** 18n }), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_2], { account: bridger.account.address });

      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      const ids = [TOKEN_ID_1, TOKEN_ID_2];
      const amounts = [5n, 5n];

      await nft.write.safeBatchTransferFrom([user1.account.address, user2.account.address, ids, amounts, "0x"], { account: user1.account.address });
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 5n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_2]), 5n);
    });

    it("Should reject transfers of Settled (soulbound) tokens", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      // Settle burns Issued and mints soulbound Settled to user1.
      await nft.write.grantRole([await nft.read.SETTLEMENT_ROLE(), deployer.account.address], {
        account: deployer.account.address,
      });
      await nft.write.settle([SERIES_ID_1, user1.account.address, user1.account.address, 4n], {
        account: deployer.account.address,
      });

      const settledTokenId = await nft.read.settledTokenId([SERIES_ID_1]);
      assert.equal(await nft.read.balanceOf([user1.account.address, settledTokenId]), 4n);

      // Settled tokens are soulbound — holder-to-holder transfer reverts.
      await viem.assertions.revertWithCustomError(
        nft.write.safeTransferFrom([user1.account.address, user2.account.address, settledTokenId, 1n, "0x"], { account: user1.account.address }),
        nft,
        "SoulboundSettled",
      );
    });
  });

  describe("Interface Support", async function () {
    it("Should support ERC1155 interface", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      // Check ERC1155 interface support
      const erc1155InterfaceId = "0xd9b67a26"; // ERC1155 interface ID
      assert(await nft.read.supportsInterface([erc1155InterfaceId]));

      // Check AccessControl interface support
      const accessControlInterfaceId = "0x7965db0b"; // AccessControl interface ID
      assert(await nft.read.supportsInterface([accessControlInterfaceId]));
    });
  });

  describe("Metadata and TokenURI", async function () {
    it("Should allow admin to set collection metadata", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const description = "Intex financial instrument NFT";
      await nft.write.setCollectionMetadata([description], { account: deployer.account.address });

      assert.equal(await nft.read.collectionDescription(), description);
    });

    it("Should prevent non-admin from setting collection metadata", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const description = "Test description";

      await viem.assertions.revertWithCustomError(
        nft.write.setCollectionMetadata([description], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should generate tokenURI with metadata", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      // Set collection description
      await nft.write.setCollectionMetadata(["Intex NFT Collection"], { account: deployer.account.address });

      // Create series and mint
      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      // Get tokenURI
      const uri = await nft.read.uri([TOKEN_ID_1]);

      // Verify it's a base64 data URI
      assert(uri.startsWith("data:application/json;base64,"));
      assert(uri.length > 100); // Should have substantial content
    });

    it("Should generate different tokenURI for different states", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.setCollectionMetadata(["Test Collection"], { account: deployer.account.address });

      // Create series and mint
      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      // Get initial URI (Issued state)
      const uri1 = await nft.read.uri([TOKEN_ID_1]);

      // Mark as called
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      // Get new URI (Called state)
      const uri2 = await nft.read.uri([TOKEN_ID_1]);

      // URIs should be different
      assert.notEqual(uri1, uri2);
    });

    it("Should revert tokenURI for non-existent token", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await viem.assertions.revertWithCustomError(
        nft.read.uri([999n]),
        nft,
        "NonexistentToken",
      );
    });

    it("Should format strike price and floor price correctly in metadata", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.setCollectionMetadata(["Test Collection"], { account: deployer.account.address });

      const strikePrice = 1500000n; // Should display as "1.50" with 6 decimals
      const priceFloor = 1000000n; // Should display as "1.00" with 6 decimals

      await nft.write.createSeries(seriesArgs(SERIES_ID_1, { strikePrice, priceFloor }), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      const uri = await nft.read.uri([TOKEN_ID_1]);

      assert(uri.startsWith("data:application/json;base64,"));

      const base64Data = uri.replace("data:application/json;base64,", "");
      const jsonStr = Buffer.from(base64Data, "base64").toString("utf8");
      const metadata = JSON.parse(jsonStr);

      assert(metadata.attributes);
      assert(Array.isArray(metadata.attributes));

      const strikePriceAttr = metadata.attributes.find((attr: any) => attr.trait_type === "Strike Price");
      const floorPriceAttr = metadata.attributes.find((attr: any) => attr.trait_type === "Floor Price");

      assert(strikePriceAttr, "Strike Price attribute should exist");
      assert(floorPriceAttr, "Floor Price attribute should exist");

      assert.equal(strikePriceAttr.value, "1.50", "Strike price should be formatted as 1.50");
      assert.equal(floorPriceAttr.value, "1.00", "Floor price should be formatted as 1.00");
    });

    it("Should format various strike and floor price values correctly", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.setCollectionMetadata(["Test Collection"], { account: deployer.account.address });

      const strikePrice1 = 2500000n; // 2.50
      const floorPrice1 = 3000000n; // 3.00

      await nft.write.createSeries(seriesArgs(SERIES_ID_1, { strikePrice: strikePrice1, priceFloor: floorPrice1 }), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      let uri = await nft.read.uri([TOKEN_ID_1]);
      let base64Data = uri.replace("data:application/json;base64,", "");
      let jsonStr = Buffer.from(base64Data, "base64").toString("utf8");
      let metadata = JSON.parse(jsonStr);

      let strikePriceAttr = metadata.attributes.find((attr: any) => attr.trait_type === "Strike Price");
      let floorPriceAttr = metadata.attributes.find((attr: any) => attr.trait_type === "Floor Price");

      assert.equal(strikePriceAttr.value, "2.50", "Strike price should be formatted as 2.50");
      assert.equal(floorPriceAttr.value, "3.00", "Floor price should be formatted as 3.00");

      const strikePrice2 = 500000n; // 0.50
      const floorPrice2 = 750000n; // 0.75

      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { strikePrice: strikePrice2, priceFloor: floorPrice2 }), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_2], { account: bridger.account.address });

      uri = await nft.read.uri([TOKEN_ID_2]);
      base64Data = uri.replace("data:application/json;base64,", "");
      jsonStr = Buffer.from(base64Data, "base64").toString("utf8");
      metadata = JSON.parse(jsonStr);

      strikePriceAttr = metadata.attributes.find((attr: any) => attr.trait_type === "Strike Price");
      floorPriceAttr = metadata.attributes.find((attr: any) => attr.trait_type === "Floor Price");

      assert.equal(strikePriceAttr.value, "0.50", "Strike price should be formatted as 0.50");
      assert.equal(floorPriceAttr.value, "0.75", "Floor price should be formatted as 0.75");
    });

    // a description full of JSON-hostile bytes must still yield valid, parseable JSON,
    // round-trip exactly through the escape, and not inject a sibling key.
    it("Should produce valid parseable JSON for a description with quotes, backslash, newline, and control chars", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      // Contains: double quote, backslash, newline, tab, a control char (0x01),
      // and an injection attempt that tries to close the string and add a sibling `image` key.
      const nasty = 'He said "hi" \\ path\nnewline\ttab  ctrl ","image":"http://evil';

      await nft.write.setCollectionMetadata([nasty], { account: deployer.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      const uri = await nft.read.uri([TOKEN_ID_1]);
      const base64Data = uri.replace("data:application/json;base64,", "");
      const jsonStr = Buffer.from(base64Data, "base64").toString("utf8");

      // JSON.parse throwing here would itself fail the test — i.e. proves the document parses.
      const metadata = JSON.parse(jsonStr);

      // The description round-trips byte-for-byte: escaping is reversible, not lossy.
      assert.equal(metadata.description, nasty, "description must round-trip exactly through escaping");
      // The only `image` is the real SVG data URI — the injection did not create a sibling key.
      assert(
        typeof metadata.image === "string" && metadata.image.startsWith("data:image/svg+xml;base64,"),
        "image must be the SVG data URI, not an injected value",
      );
    });
  });

  describe("Token ID Helpers", async function () {
    it("Should derive the issued token id from a series id", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      assert.equal(await nft.read.issuedTokenId([SERIES_ID_1]), TOKEN_ID_1);
    });

    it("Should derive both token ids for a series", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const [issued, settled] = await nft.read.tokenIds([SERIES_ID_1]);
      assert.equal(issued, TOKEN_ID_1);
      assert.equal(settled, await nft.read.settledTokenId([SERIES_ID_1]));
      // Issued and Settled token ids are distinct.
      assert.notEqual(issued, settled);
    });

    it("Should report token status per token id", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      const issued = await nft.read.issuedTokenId([SERIES_ID_1]);
      const settled = await nft.read.settledTokenId([SERIES_ID_1]);
      assert.equal(await nft.read.statusOf([issued]), 0); // Issued
      assert.equal(await nft.read.statusOf([settled]), 1); // Settled
    });
  });

  describe("CrosschainBurn/CrosschainMint", async function () {
    it("Should allow bridge to crosschainBurn tokens in Qualified state", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const quantity = 10n;
      const burnAmount = 5n;

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, quantity, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, burnAmount], { account: bridger.account.address });

      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), quantity - burnAmount);
    });

    it("Should allow crosschainBurn in Called state for SYSTEM_RELAYER_ROLE", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      // Plain RELAYER cannot crosschainBurn while Called.
      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: bridger.account.address }),
        nft,
        "BridgeStateForbidden",
      );

      await nft.write.grantRole([await nft.read.SYSTEM_RELAYER_ROLE(), bridger.account.address], {
        account: deployer.account.address,
      });
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: bridger.account.address });

      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 5n);
    });

    it("Should prevent non-bridge from crosschainBurning", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should allow bridge to crosschainMint tokens in Qualified state", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const mintAmount = 10n;

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });
      await nft.write.crosschainMint([user1.account.address, TOKEN_ID_1, mintAmount], { account: bridger.account.address });

      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), mintAmount);
    });

    it("Should allow crosschainMint in Called state for SYSTEM_RELAYER_ROLE", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.crosschainMint([user1.account.address, TOKEN_ID_1, 10n], { account: bridger.account.address }),
        nft,
        "BridgeStateForbidden",
      );

      await nft.write.grantRole([await nft.read.SYSTEM_RELAYER_ROLE(), bridger.account.address], {
        account: deployer.account.address,
      });
      await nft.write.crosschainMint([user1.account.address, TOKEN_ID_1, 10n], { account: bridger.account.address });

      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 10n);
    });

    it("Should prevent non-bridge from crosschainMinting", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.crosschainMint([user1.account.address, TOKEN_ID_1, 10n], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should revert crosschainMint for non-existent series", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await viem.assertions.revertWithCustomError(
        nft.write.crosschainMint([user1.account.address, TOKEN_ID_1, 10n], { account: bridger.account.address }),
        nft,
        "NonexistentToken",
      );
    });

    it("Should reject bridge crosschainBurn on a Settled token id", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      const settledTokenId = await nft.read.settledTokenId([SERIES_ID_1]);

      await viem.assertions.revertWithCustomError(
        nft.write.crosschainBurn([user1.account.address, settledTokenId, 1n], { account: bridger.account.address }),
        nft,
        "BridgeOnSettledForbidden",
      );
    });
  });

  describe("Settle and Burn Settled", async function () {
    it("Should settle: burn Issued from holder and mint soulbound Settled", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      await nft.write.grantRole([await nft.read.SETTLEMENT_ROLE(), deployer.account.address], {
        account: deployer.account.address,
      });
      await nft.write.settle([SERIES_ID_1, user1.account.address, user1.account.address, 6n], {
        account: deployer.account.address,
      });

      const settledTokenId = await nft.read.settledTokenId([SERIES_ID_1]);
      // Issued burned, Settled minted.
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 4n);
      assert.equal(await nft.read.balanceOf([user1.account.address, settledTokenId]), 6n);

      const balances = await nft.read.holderBalances([SERIES_ID_1, user1.account.address]);
      assert.equal(balances.issued, 4);
      assert.equal(balances.settled, 6);
    });

    it("Should prevent non-settlement role from settling", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.settle([SERIES_ID_1, user1.account.address, user1.account.address, 1n], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should reject settle while the series is still Issued", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      await nft.write.grantRole([await nft.read.SETTLEMENT_ROLE(), deployer.account.address], {
        account: deployer.account.address,
      });
      // Series in Issued state — settle is not allowed.
      await viem.assertions.revertWithCustomError(
        nft.write.settle([SERIES_ID_1, user1.account.address, user1.account.address, 1n], { account: deployer.account.address }),
        nft,
        "InvalidStateForSettle",
      );
    });

    it("Should burn Settled tokens via burnSettled under PROMIS_ROLE", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      await nft.write.grantRole([await nft.read.SETTLEMENT_ROLE(), deployer.account.address], {
        account: deployer.account.address,
      });
      await nft.write.settle([SERIES_ID_1, user1.account.address, user1.account.address, 8n], {
        account: deployer.account.address,
      });

      await nft.write.grantRole([await nft.read.PROMIS_ROLE(), deployer.account.address], {
        account: deployer.account.address,
      });
      await nft.write.burnSettled([user1.account.address, SERIES_ID_1, 3n], { account: deployer.account.address });

      const settledTokenId = await nft.read.settledTokenId([SERIES_ID_1]);
      assert.equal(await nft.read.balanceOf([user1.account.address, settledTokenId]), 5n);
    });

    it("Should prevent non-promis role from burning Settled tokens", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      await nft.write.grantRole([await nft.read.SETTLEMENT_ROLE(), deployer.account.address], {
        account: deployer.account.address,
      });
      await nft.write.settle([SERIES_ID_1, user1.account.address, user1.account.address, 4n], {
        account: deployer.account.address,
      });

      await viem.assertions.revertWithCustomError(
        nft.write.burnSettled([user1.account.address, SERIES_ID_1, 1n], { account: user1.account.address }),
        nft,
        "AccessControlUnauthorizedAccount",
      );
    });
  });

  describe("Enumerable Functions", async function () {
    it("Should track all series created via getAllSeries and totalSeries", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      // Initially empty
      assert.equal(await nft.read.totalSeries(), 0n);
      const initialSeries = await nft.read.getAllSeries();
      assert.equal(initialSeries.length, 0);

      // Create first series
      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      assert.equal(await nft.read.totalSeries(), 1n);

      // Create second series
      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { promisLoadMinor: 200n * 10n ** 18n }), { account: bridger.account.address });
      assert.equal(await nft.read.totalSeries(), 2n);

      // Get all series
      const allSeries = await nft.read.getAllSeries();
      assert.equal(allSeries.length, 2);
      assert.equal(allSeries[0], TOKEN_ID_1);
      assert.equal(allSeries[1], TOKEN_ID_2);
    });

    it("Should support pagination via getSeriesPaginated", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { promisLoadMinor: 200n * 10n ** 18n }), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_3, { promisLoadMinor: 300n * 10n ** 18n }), { account: bridger.account.address });

      // Get first page
      const [page1, total1] = await nft.read.getSeriesPaginated([0n, 2n]);
      assert.equal(total1, 3n);
      assert.equal(page1.length, 2);
      assert.equal(page1[0], TOKEN_ID_1);
      assert.equal(page1[1], TOKEN_ID_2);

      // Get second page
      const [page2, total2] = await nft.read.getSeriesPaginated([2n, 2n]);
      assert.equal(total2, 3n);
      assert.equal(page2.length, 1);
      assert.equal(page2[0], TOKEN_ID_3);

      // Offset beyond length returns empty
      const [page3, total3] = await nft.read.getSeriesPaginated([10n, 2n]);
      assert.equal(total3, 3n);
      assert.equal(page3.length, 0);
    });

    it("Should track owned series per address via getOwnedSeries and ownedSeriesCount", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      // Create two series
      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { promisLoadMinor: 200n * 10n ** 18n }), { account: bridger.account.address });

      // Initially user has no tokens
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 0n);
      const initialOwned = await nft.read.getOwnedSeries([user1.account.address]);
      assert.equal(initialOwned.length, 0);

      // Mint first series to user
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);

      // Mint second series to user
      await nft.write.mint([user1.account.address, 5n, SERIES_ID_2], { account: bridger.account.address });
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 2n);

      // Get owned series
      const ownedSeries = await nft.read.getOwnedSeries([user1.account.address]);
      assert.equal(ownedSeries.length, 2);
      assert(ownedSeries.includes(TOKEN_ID_1));
      assert(ownedSeries.includes(TOKEN_ID_2));
    });

    it("Should track total balance across all series via totalBalance", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { promisLoadMinor: 200n * 10n ** 18n }), { account: bridger.account.address });

      // Initially zero
      assert.equal(await nft.read.totalBalance([user1.account.address]), 0n);

      // Mint to user
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      assert.equal(await nft.read.totalBalance([user1.account.address]), 10n);

      await nft.write.mint([user1.account.address, 5n, SERIES_ID_2], { account: bridger.account.address });
      assert.equal(await nft.read.totalBalance([user1.account.address]), 15n);

      // Additional mint to same series should add up
      await nft.write.mint([user1.account.address, 3n, SERIES_ID_1], { account: bridger.account.address });
      assert.equal(await nft.read.totalBalance([user1.account.address]), 18n);
    });

    it("Should update owned series on full transfer", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      // User1 has 1 series, user2 has 0
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.ownedSeriesCount([user2.account.address]), 0n);

      // Transfer all to user2
      await nft.write.safeTransferFrom([user1.account.address, user2.account.address, TOKEN_ID_1, 10n, "0x"], { account: user1.account.address });

      // User1 should have 0 series now, user2 should have 1
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 0n);
      assert.equal(await nft.read.ownedSeriesCount([user2.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 0n);
      assert.equal(await nft.read.totalBalance([user2.account.address]), 10n);

      // Verify getOwnedSeries reflects the change
      const user1Owned = await nft.read.getOwnedSeries([user1.account.address]);
      const user2Owned = await nft.read.getOwnedSeries([user2.account.address]);
      assert.equal(user1Owned.length, 0);
      assert.equal(user2Owned.length, 1);
      assert.equal(user2Owned[0], TOKEN_ID_1);
    });

    it("Should handle partial transfers correctly", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      // Partial transfer
      await nft.write.safeTransferFrom([user1.account.address, user2.account.address, TOKEN_ID_1, 5n, "0x"], { account: user1.account.address });

      // Both users should still own the series
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.ownedSeriesCount([user2.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 5n);
      assert.equal(await nft.read.totalBalance([user2.account.address]), 5n);
    });

    it("Should handle crosschainBurns correctly for enumeration", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 10n);

      // Partial crosschainBurn - should still own the series
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: bridger.account.address });
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 5n);

      // Full crosschainBurn - should no longer own the series
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: bridger.account.address });
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 0n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 0n);

      const ownedAfterBurn = await nft.read.getOwnedSeries([user1.account.address]);
      assert.equal(ownedAfterBurn.length, 0);
    });

    it("Should return owned series with balances via getOwnedSeriesWithBalances", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { promisLoadMinor: 200n * 10n ** 18n }), { account: bridger.account.address });

      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 25n, SERIES_ID_2], { account: bridger.account.address });

      const [tokenIds, balances] = await nft.read.getOwnedSeriesWithBalances([user1.account.address]);

      assert.equal(tokenIds.length, 2);
      assert.equal(balances.length, 2);

      // Find index for each token
      const idx1 = tokenIds.findIndex((id: bigint) => id === TOKEN_ID_1);
      const idx2 = tokenIds.findIndex((id: bigint) => id === TOKEN_ID_2);

      assert(idx1 >= 0, "TOKEN_ID_1 should be in tokenIds");
      assert(idx2 >= 0, "TOKEN_ID_2 should be in tokenIds");
      assert.equal(balances[idx1], 10n);
      assert.equal(balances[idx2], 25n);
    });

    it("Should handle mintBatch correctly for enumeration", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      const recipients = [user1.account.address, user2.account.address];
      const quantities = [5n, 10n];

      await nft.write.mintBatch([recipients, quantities, SERIES_ID_1], { account: bridger.account.address });

      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.ownedSeriesCount([user2.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 5n);
      assert.equal(await nft.read.totalBalance([user2.account.address]), 10n);
    });

    it("Should handle multiple series ownership correctly", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_2, { promisLoadMinor: 200n * 10n ** 18n }), { account: bridger.account.address });
      await nft.write.createSeries(seriesArgs(SERIES_ID_3, { promisLoadMinor: 300n * 10n ** 18n }), { account: bridger.account.address });

      // Mint all 3 to user1
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 20n, SERIES_ID_2], { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 30n, SERIES_ID_3], { account: bridger.account.address });

      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 3n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 60n);

      // Transfer middle one completely
      await nft.write.safeTransferFrom([user1.account.address, user2.account.address, TOKEN_ID_2, 20n, "0x"], { account: user1.account.address });

      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 2n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 40n);
      assert.equal(await nft.read.ownedSeriesCount([user2.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user2.account.address]), 20n);

      // Verify correct series are owned
      const user1Owned = await nft.read.getOwnedSeries([user1.account.address]);
      assert.equal(user1Owned.length, 2);
      assert(user1Owned.includes(TOKEN_ID_1));
      assert(user1Owned.includes(TOKEN_ID_3));
      assert(!user1Owned.includes(TOKEN_ID_2));
    });

    it("Should handle crosschainMint/crosschainBurn for enumeration", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.markQualified([SERIES_ID_1], { account: bridger.account.address });

      // CrosschainMint (like receiving from another chain)
      await nft.write.crosschainMint([user1.account.address, TOKEN_ID_1, 15n], { account: bridger.account.address });
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 15n);

      // CrosschainBurn partial
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 5n], { account: bridger.account.address });
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 10n);

      // CrosschainBurn full
      await nft.write.crosschainBurn([user1.account.address, TOKEN_ID_1, 10n], { account: bridger.account.address });
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 0n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 0n);
    });

    it("Should not add duplicate entries when minting to same user multiple times", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });

      // Mint multiple times to same user
      await nft.write.mint([user1.account.address, 5n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 15n, SERIES_ID_1], { account: bridger.account.address });

      // Should still only have 1 series entry
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 30n);

      const ownedSeries = await nft.read.getOwnedSeries([user1.account.address]);
      assert.equal(ownedSeries.length, 1);
      assert.equal(ownedSeries[0], TOKEN_ID_1);
    });

    it("Should handle batch transfer with duplicate tokenIds correctly", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });

      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
      assert.equal(await nft.read.ownedSeriesCount([user2.account.address]), 0n);

      // Batch transfer with same tokenId appearing twice: [id, id] with amounts [5, 5]
      // This tests the fix for duplicate tokenId handling in _update
      await nft.write.safeBatchTransferFrom(
        [user1.account.address, user2.account.address, [TOKEN_ID_1, TOKEN_ID_1], [5n, 5n], "0x"],
        { account: user1.account.address }
      );

      // user1 should have 0 balance and no owned series
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 0n);
      assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 0n);
      assert.equal(await nft.read.totalBalance([user1.account.address]), 0n);

      // user2 should have 10 balance and 1 owned series
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 10n);
      assert.equal(await nft.read.ownedSeriesCount([user2.account.address]), 1n);
      assert.equal(await nft.read.totalBalance([user2.account.address]), 10n);

      const user1Owned = await nft.read.getOwnedSeries([user1.account.address]);
      const user2Owned = await nft.read.getOwnedSeries([user2.account.address]);
      assert.equal(user1Owned.length, 0);
      assert.equal(user2Owned.length, 1);
      assert.equal(user2Owned[0], TOKEN_ID_1);
    });
  });

  describe("Expire Series", async function () {
    it("Should expire series after deadline and burn all NFTs", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const customCallPeriod = 86400n; // 1 day

      await nft.write.createSeries(seriesArgs(SERIES_ID_1, { callPeriod: customCallPeriod }), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.mint([user2.account.address, 5n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 10n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 5n);

      const data0 = await nft.read.readData([SERIES_ID_1]);
      const derivedDeadline = BigInt(data0.calledAt) + BigInt(data0.intexCallPeriod);
      await testClient.setNextBlockTimestamp({ timestamp: derivedDeadline + 1n });
      await testClient.mine({ blocks: 1 });

      await nft.write.expireSeries([SERIES_ID_1, EXPIRE_FULL_LIMIT], { account: bridger.account.address });

      const data = await nft.read.readData([SERIES_ID_1]);
      // Expiration is event-only — series state stays Called (=2).
      assert.equal(data.state, 2);
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 0n);
      assert.equal(await nft.read.balanceOf([user2.account.address, TOKEN_ID_1]), 0n);
      assert.equal(await nft.read.totalSupply([TOKEN_ID_1]), 0n);
      assert.equal(data.costAmountMinor, STRIKE_PRICE);
    });

    it("Should revert expire before deadline", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      await nft.write.createSeries(seriesArgs(SERIES_ID_1), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      await viem.assertions.revertWithCustomError(
        nft.write.expireSeries([SERIES_ID_1, EXPIRE_FULL_LIMIT], { account: bridger.account.address }),
        nft,
        "SeriesNotYetExpired",
      );
    });

    it("Should burn Issued balances on expiration (no transferable supply remains)", async function () {
      const nft = await deployIntexNFT1155(viem, [deployer.account.address, bridger.account.address]);

      const customCallPeriod = 86400n; // 1 day

      await nft.write.createSeries(seriesArgs(SERIES_ID_1, { callPeriod: customCallPeriod }), { account: bridger.account.address });
      await nft.write.mint([user1.account.address, 10n, SERIES_ID_1], { account: bridger.account.address });
      await nft.write.markCalled([SERIES_ID_1], { account: bridger.account.address });

      const data0 = await nft.read.readData([SERIES_ID_1]);
      const derivedDeadline = BigInt(data0.calledAt) + BigInt(data0.intexCallPeriod);
      await testClient.setNextBlockTimestamp({ timestamp: derivedDeadline + 1n });
      await testClient.mine({ blocks: 1 });

      await nft.write.expireSeries([SERIES_ID_1, EXPIRE_FULL_LIMIT], { account: bridger.account.address });

      // expireSeries sweeps all Issued balances to zero — there's nothing left to transfer.
      assert.equal(await nft.read.balanceOf([user1.account.address, TOKEN_ID_1]), 0n);
      assert.equal(await nft.read.totalSupply([TOKEN_ID_1]), 0n);
    });
  });

});
