/**
 * ONFT1155Adapter Hardhat Tests
 *
 * Note: Full cross-chain tests are in test/foundry/cross-chain/ONFT1155Adapter.t.sol (Foundry)
 * These tests cover basic deployment and configuration.
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { network } from "hardhat";
import { deployIntexNFT1155 } from "../_helpers.js";

describe("ONFT1155Adapter", async function () {
  const { viem } = await network.connect();

  // Test accounts
  const [deployer, bridger, user1] = await viem.getWalletClients();

  const SERIES_ID = 20250101;
  const TOKEN_ID = BigInt(SERIES_ID);
  const AMOUNT = 100n;

  const INTEX_SIZE = 100n * 10n ** 18n;
  const INTEX_STRIKE_PRICE = 1000n * 10n ** 6n;
  const COEN_PRICE_FLOOR = 500n * 10n ** 6n;
  const SETTLEMENT_TOKEN_ALIAS = 840;
  const CALL_TRIGGER = {
    windowDays: 0,
    thresholdDays: 0,
    coenPriceCallTrigger: 0n,
  };

  async function createSeries(
    nft: Awaited<ReturnType<typeof deployIntexNFT1155>>,
  ) {
    await nft.write.createSeries([SERIES_ID, AMOUNT, 0], { account: bridger.account.address });
  }

  it("Should deploy IntexNFT1155 with IERC1155Bridgeable interface", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    // Verify RELAYER_ROLE is set
    const RELAYER_ROLE = await nft.read.RELAYER_ROLE();
    assert(await nft.read.hasRole([RELAYER_ROLE, bridger.account.address]));
  });

  it("Should have crosschainBurn and crosschainMint functions for bridge compatibility", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    // Create series and mint tokens
    await createSeries(nft);
    await nft.write.mint([user1.account.address, AMOUNT, SERIES_ID], {
      account: bridger.account.address,
    });
    // Bridge crosschainBurn/crosschainMint are gated on series state; Qualified is the user-driven bridge window.
    await nft.write.markQualified([SERIES_ID], { account: bridger.account.address });

    // Verify initial balance
    assert.equal(
      await nft.read.balanceOf([user1.account.address, TOKEN_ID]),
      AMOUNT
    );

    // Test crosschainBurn (burn)
    await nft.write.crosschainBurn([user1.account.address, TOKEN_ID, 50n], {
      account: bridger.account.address,
    });
    assert.equal(
      await nft.read.balanceOf([user1.account.address, TOKEN_ID]),
      50n
    );

    // Test crosschainMint (mint)
    await nft.write.crosschainMint([user1.account.address, TOKEN_ID, 25n], {
      account: bridger.account.address,
    });
    assert.equal(
      await nft.read.balanceOf([user1.account.address, TOKEN_ID]),
      75n
    );
  });

  it("Should only allow RELAYER_ROLE to call crosschainBurn", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    await createSeries(nft);
    await nft.write.mint([user1.account.address, AMOUNT, SERIES_ID], {
      account: bridger.account.address,
    });

    // Non-bridge should not be able to crosschainBurn
    await viem.assertions.revertWithCustomError(
      nft.write.crosschainBurn([user1.account.address, TOKEN_ID, 50n], {
        account: user1.account.address,
      }),
      nft,
      "AccessControlUnauthorizedAccount"
    );
  });

  it("Should only allow RELAYER_ROLE to call crosschainMint", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    await createSeries(nft);

    // Non-bridge should not be able to crosschainMint
    await viem.assertions.revertWithCustomError(
      nft.write.crosschainMint([user1.account.address, TOKEN_ID, 50n], {
        account: user1.account.address,
      }),
      nft,
      "AccessControlUnauthorizedAccount"
    );
  });

  it("Should revert crosschainBurn on non-existent token", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    // Try to crosschainBurn token that doesn't exist
    await viem.assertions.revertWithCustomError(
      nft.write.crosschainBurn([user1.account.address, TOKEN_ID, 50n], {
        account: bridger.account.address,
      }),
      nft,
      "NonexistentToken"
    );
  });

  it("Should revert crosschainMint to zero address", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    await createSeries(nft);

    // Try to crosschainMint to zero address
    await viem.assertions.revertWithCustomError(
      nft.write.crosschainMint(
        ["0x0000000000000000000000000000000000000000", TOKEN_ID, 50n],
        { account: bridger.account.address }
      ),
      nft,
      "ZeroAddress"
    );
  });

  it("Should track totalSupply correctly through crosschainBurn/crosschainMint", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    await createSeries(nft);

    // Initial mint
    await nft.write.mint([user1.account.address, AMOUNT, SERIES_ID], {
      account: bridger.account.address,
    });
    await nft.write.markQualified([SERIES_ID], { account: bridger.account.address });
    assert.equal(await nft.read.totalSupply([TOKEN_ID]), AMOUNT);

    // CrosschainBurn should decrease totalSupply
    await nft.write.crosschainBurn([user1.account.address, TOKEN_ID, 30n], {
      account: bridger.account.address,
    });
    assert.equal(await nft.read.totalSupply([TOKEN_ID]), 70n);

    // CrosschainMint should increase totalSupply
    await nft.write.crosschainMint([user1.account.address, TOKEN_ID, 20n], {
      account: bridger.account.address,
    });
    assert.equal(await nft.read.totalSupply([TOKEN_ID]), 90n);
  });

  it("Should maintain enumerable tracking through crosschainBurn/crosschainMint", async function () {
    const nft = await deployIntexNFT1155(viem, [
      deployer.account.address,
      bridger.account.address,
    ]);

    await createSeries(nft);
    await nft.write.markQualified([SERIES_ID], { account: bridger.account.address });

    // CrosschainMint tokens to user (simulating bridge receive)
    await nft.write.crosschainMint([user1.account.address, TOKEN_ID, AMOUNT], {
      account: bridger.account.address,
    });

    // User should now own this series
    assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 1n);
    assert.equal(await nft.read.totalBalance([user1.account.address]), AMOUNT);

    // CrosschainBurn all tokens (simulating bridge send)
    await nft.write.crosschainBurn([user1.account.address, TOKEN_ID, AMOUNT], {
      account: bridger.account.address,
    });

    // User should no longer own this series
    assert.equal(await nft.read.ownedSeriesCount([user1.account.address]), 0n);
    assert.equal(await nft.read.totalBalance([user1.account.address]), 0n);
  });
});

/**
 * Note: Full cross-chain integration tests for ONFT1155Adapter are in Foundry:
 * test/foundry/cross-chain/ONFT1155Adapter.t.sol
 *
 * Those tests use LayerZero's TestHelperOz5 to simulate cross-chain messaging
 * with EndpointV2Mock contracts.
 */
