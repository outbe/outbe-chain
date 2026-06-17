import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { network } from "hardhat";
import { encodePacked, keccak256, sha256, zeroAddress } from "viem";
import { deployIntexNFT1155 } from "./_helpers.js";

describe("IntexSettlement", async function () {
  const { viem } = await network.connect();
  const testClient = await viem.getTestClient();

  const [deployer, _, __, holder, payer] = await viem.getWalletClients();

  const SERIES_ID = 20250101;
  const ISSUED_INTEX_COUNT = 10_000;
  const TOKEN_ID = BigInt(SERIES_ID);
  const PROMIS_LOAD_MINOR = 1000n;
  const STRIKE_PRICE = 100n * 10n ** 6n;
  const FLOOR_PRICE_MINOR = 40n * 10n ** 6n;
  const REFERENCE_CURRENCY = 840;
  const CALL_TRIGGER = {
    windowDays: 0,
    thresholdDays: 0,
    callPriceMinor: 0n,
  };

  async function deployContracts() {
    const intex = await deployIntexNFT1155(viem, [
      deployer.account.address,
      deployer.account.address,
    ]);

    const promis = await viem.deployContract("MockPromis", []);

    const usdc = await viem.deployContract("MockERC20", [
      "USD Coin",
      "USDC",
      6,
    ]);

    const vault = await viem.deployContract("MockSettlementVault", [
      usdc.address,
      "Morpho Vault USDC",
      "mvUSDC",
      6,
    ]);

    const vaultProvider = await viem.deployContract("MockVaultProvider", []);
    await vaultProvider.write.addVault([vault.address]);

    const settlement = await viem.deployContract("IntexSettlement", [
      deployer.account.address,
    ]);

    const settlementRoleOnIntex = await intex.read.SETTLEMENT_ROLE();
    await intex.write.grantRole([settlementRoleOnIntex, settlement.address]);

    const promisRoleOnIntex = await intex.read.PROMIS_ROLE();
    await intex.write.grantRole([promisRoleOnIntex, settlement.address]);

    // Register settlement as a permitted depositor on the provider before wiring.
    // Slot 2 = IntexStrikePrice in the LiquiditySource enum.
    await vaultProvider.write.addLiquiditySource([settlement.address, 2]);
    await settlement.write.wire([intex.address, vaultProvider.address, usdc.address, promis.address]);

    return { intex, promis, usdc, vault, vaultProvider, settlement };
  }

  async function settledTokenId(
    intex: Awaited<ReturnType<typeof deployContracts>>["intex"],
  ): Promise<bigint> {
    return intex.read.settledTokenId([SERIES_ID]);
  }

  async function createSeries(
    intex: Awaited<ReturnType<typeof deployContracts>>["intex"],
    intexCallPeriod: bigint,
  ) {
    await intex.write.createSeries([SERIES_ID, ISSUED_INTEX_COUNT, Number(intexCallPeriod)]);
  }

  async function setupCalledSeries(
    intex: Awaited<ReturnType<typeof deployContracts>>["intex"],
    holderAddr: `0x${string}`,
    amount: bigint,
  ) {
    const callPeriod = 86400n;
    await createSeries(intex, callPeriod);
    await intex.write.mint([holderAddr, amount, SERIES_ID]);

    await intex.write.markCalled([SERIES_ID]);
    const data = await intex.read.readData([SERIES_ID]);
    return BigInt(data.calledAt) + BigInt(data.intexCallPeriod);
  }

  async function fundAndApprove(
    usdc: Awaited<ReturnType<typeof deployContracts>>["usdc"],
    settlement: Awaited<ReturnType<typeof deployContracts>>["settlement"],
    who: `0x${string}`,
    amount: bigint,
  ) {
    await usdc.write.mint([who, amount]);
    await usdc.write.approve([settlement.address, amount], { account: who });
  }

  describe("Deployment", async function () {
    it("Should deploy with correct initial state", async function () {
      const { intex, vaultProvider, usdc, settlement } = await deployContracts();

      assert.equal((await settlement.read.intex()).toLowerCase(), intex.address.toLowerCase());
      assert.equal(
        (await settlement.read.vaultProvider()).toLowerCase(),
        vaultProvider.address.toLowerCase(),
      );
      assert.equal((await settlement.read.paymentToken()).toLowerCase(), usdc.address.toLowerCase());
      assert(
        await settlement.read.hasRole([
          await settlement.read.DEFAULT_ADMIN_ROLE(),
          deployer.account.address,
        ]),
      );
    });

    it("Should revert on zero admin", async function () {
      const { settlement } = await deployContracts();

      await viem.assertions.revertWithCustomError(
        viem.deployContract("IntexSettlement", [zeroAddress]),
        settlement,
        "ZeroAddress",
      );
    });
  });

  describe("Same Wallet Settlement", async function () {
    it("Should settle full balance: stablecoins to vault, shares to provider, Issued burned, Settled minted", async function () {
      const { intex, promis, usdc, vault, vaultProvider, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);

      const payment = STRIKE_PRICE * 10n;
      await fundAndApprove(usdc, settlement, holder.account.address, payment);

      await settlement.write.settle(
        [SERIES_ID, holder.account.address, 10n],
        { account: holder.account.address },
      );

      const sTok = await settledTokenId(intex);
      assert.equal(await usdc.read.balanceOf([vault.address]), payment);
      // Shares accrue on the VaultProvider after migration, not on the settlement contract.
      assert.equal(await vault.read.balanceOf([vaultProvider.address]), payment);
      assert.equal(await vault.read.balanceOf([settlement.address]), 0n);
      assert.equal(await intex.read.balanceOf([holder.account.address, TOKEN_ID]), 0n);
      assert.equal(await intex.read.balanceOf([holder.account.address, sTok]), 10n);
      assert.equal(await intex.read.totalSupply([TOKEN_ID]), 0n);
      // Promis is not minted at settle time anymore.
      assert.equal(await promis.read.balanceOf([holder.account.address]), 0n);
    });
  });

  describe("Different Wallet Settlement", async function () {
    it("Should settle when authorized: Issued from holder, stablecoins from payer, Settled to settler (payer)", async function () {
      const { intex, promis, usdc, vault, vaultProvider, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);

      const payment = STRIKE_PRICE * 10n;
      await fundAndApprove(usdc, settlement, payer.account.address, payment);

      await settlement.write.authorizeSettler(
        [SERIES_ID, payer.account.address],
        { account: holder.account.address },
      );

      await settlement.write.settle(
        [SERIES_ID, holder.account.address, 10n],
        { account: payer.account.address },
      );

      const sTok = await settledTokenId(intex);
      assert.equal(await usdc.read.balanceOf([vault.address]), payment);
      assert.equal(await vault.read.balanceOf([vaultProvider.address]), payment);
      // Holder's Issued burned, Settled minted to the settler (payer).
      assert.equal(await intex.read.balanceOf([holder.account.address, TOKEN_ID]), 0n);
      assert.equal(await intex.read.balanceOf([holder.account.address, sTok]), 0n);
      assert.equal(await intex.read.balanceOf([payer.account.address, sTok]), 10n);
      // Promis is minted later by whoever holds Settled — that is now the settler.
      assert.equal(await promis.read.balanceOf([payer.account.address]), 0n);
      assert.equal(await promis.read.balanceOf([holder.account.address]), 0n);
    });
  });

  async function minePoW(
    settlement: any,
    who: `0x${string}`,
    seriesId: number,
    amount: bigint,
  ): Promise<bigint> {
    const promisAmount = amount * PROMIS_LOAD_MINOR;
    const seq = await settlement.read.mineSeq([seriesId, who]);
    const seed = keccak256(
      encodePacked(
        ["address", "uint256", "uint32", "uint32"],
        [who, promisAmount, seriesId, seq],
      ),
    );
    for (let nonce = 0n; nonce < 1_000_000n; nonce++) {
      const h = sha256(encodePacked(["bytes32", "uint64"], [seed, nonce]));
      if (h.startsWith("0x00")) return nonce;
    }
    throw new Error("PoW nonce not found");
  }

  describe("Holder-driven minePromis", async function () {
    it("Holder mines Promis after settle and Settled is burned", async function () {
      const { intex, promis, usdc, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 7n);
      await fundAndApprove(usdc, settlement, holder.account.address, STRIKE_PRICE * 7n);

      await settlement.write.settle(
        [SERIES_ID, holder.account.address, 7n],
        { account: holder.account.address },
      );

      const sTok = await settledTokenId(intex);
      assert.equal(await intex.read.balanceOf([holder.account.address, sTok]), 7n);
      assert.equal(await promis.read.balanceOf([holder.account.address]), 0n);

      const nonce = await minePoW(settlement, holder.account.address, SERIES_ID, 7n);
      await settlement.write.minePromis([SERIES_ID, 7n, nonce], {
        account: holder.account.address,
      });

      assert.equal(await intex.read.balanceOf([holder.account.address, sTok]), 0n);
      assert.equal(await promis.read.balanceOf([holder.account.address]), 7n * PROMIS_LOAD_MINOR);
    });

    it("Reverts when caller does not have enough Settled", async function () {
      const { intex, promis, usdc, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 5n);
      await fundAndApprove(usdc, settlement, holder.account.address, STRIKE_PRICE * 3n);

      await settlement.write.settle(
        [SERIES_ID, holder.account.address, 3n],
        { account: holder.account.address },
      );

      const nonce = await minePoW(settlement, holder.account.address, SERIES_ID, 4n);
      await viem.assertions.revertWithCustomError(
        settlement.write.minePromis([SERIES_ID, 4n, nonce], {
          account: holder.account.address,
        }),
        settlement,
        "InsufficientSettled",
      );
    });
  });

  describe("authorizeSettler", async function () {
    it("Should set and read authorized settler", async function () {
      const { settlement } = await deployContracts();

      await settlement.write.authorizeSettler(
        [SERIES_ID, payer.account.address],
        { account: holder.account.address },
      );

      assert.equal(
        (await settlement.read.authorizedSettler([holder.account.address, SERIES_ID])).toLowerCase(),
        payer.account.address.toLowerCase(),
      );
    });

    it("Should revoke with address(0)", async function () {
      const { intex, usdc, settlement } = await deployContracts();

      await settlement.write.authorizeSettler(
        [SERIES_ID, payer.account.address],
        { account: holder.account.address },
      );
      await settlement.write.authorizeSettler([SERIES_ID, zeroAddress], {
        account: holder.account.address,
      });

      assert.equal(
        await settlement.read.authorizedSettler([holder.account.address, SERIES_ID]),
        zeroAddress,
      );

      await setupCalledSeries(intex, holder.account.address, 10n);
      await fundAndApprove(usdc, settlement, payer.account.address, STRIKE_PRICE * 10n);

      await viem.assertions.revertWithCustomError(
        settlement.write.settle(
          [SERIES_ID, holder.account.address, 10n],
          { account: payer.account.address },
        ),
        settlement,
        "NotAuthorized",
      );
    });
  });

  // `previewSettle` was dropped in the VaultProvider migration — see contract NatSpec and
  // docs/adr/0003-vault-provider-integration.md (frontends compute `costAmountMinor * balance`
  // directly and query the active VaultV2 for share rate themselves).

  describe("Reverts", async function () {
    it("Should revert on zero balance", async function () {
      const { intex, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);

      await viem.assertions.revertWithCustomError(
        settlement.write.settle([SERIES_ID, payer.account.address, 1n], {
          account: payer.account.address,
        }),
        settlement,
        "ZeroBalance",
      );
    });

    it("Should revert on zero amount", async function () {
      const { intex, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);

      await viem.assertions.revertWithCustomError(
        settlement.write.settle([SERIES_ID, holder.account.address, 0n], {
          account: holder.account.address,
        }),
        settlement,
        "ZeroAmount",
      );
    });

    it("Should revert when amount exceeds balance", async function () {
      const { intex, usdc, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);
      await fundAndApprove(usdc, settlement, holder.account.address, STRIKE_PRICE * 11n);

      await viem.assertions.revertWithCustomError(
        settlement.write.settle([SERIES_ID, holder.account.address, 11n], {
          account: holder.account.address,
        }),
        settlement,
        "AmountExceedsBalance",
      );
    });

    it("Should revert on zero intexHolder", async function () {
      const { intex, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);

      await viem.assertions.revertWithCustomError(
        settlement.write.settle([SERIES_ID, zeroAddress, 1n], {
          account: holder.account.address,
        }),
        settlement,
        "ZeroAddress",
      );
    });

    it("Should revert when series not called", async function () {
      const { intex, usdc, settlement } = await deployContracts();

      await createSeries(intex, 0n);
      await intex.write.mint([holder.account.address, 10n, SERIES_ID]);
      await fundAndApprove(usdc, settlement, holder.account.address, STRIKE_PRICE * 10n);

      await viem.assertions.revertWithCustomError(
        settlement.write.settle(
          [SERIES_ID, holder.account.address, 10n],
          { account: holder.account.address },
        ),
        settlement,
        "NotCalled",
      );
    });

    it("Should revert when deadline expired", async function () {
      const { intex, usdc, settlement } = await deployContracts();
      const callDeadline = await setupCalledSeries(intex, holder.account.address, 10n);
      await fundAndApprove(usdc, settlement, holder.account.address, STRIKE_PRICE * 10n);

      await testClient.setNextBlockTimestamp({ timestamp: callDeadline + 1n });
      await testClient.mine({ blocks: 1 });

      await viem.assertions.revertWithCustomError(
        settlement.write.settle(
          [SERIES_ID, holder.account.address, 10n],
          { account: holder.account.address },
        ),
        settlement,
        "DeadlineExpired",
      );
    });

    it("Should revert when not authorized", async function () {
      const { intex, usdc, settlement } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);
      await fundAndApprove(usdc, settlement, payer.account.address, STRIKE_PRICE * 10n);

      await viem.assertions.revertWithCustomError(
        settlement.write.settle(
          [SERIES_ID, holder.account.address, 10n],
          { account: payer.account.address },
        ),
        settlement,
        "NotAuthorized",
      );
    });

    it("Should revert settle when not wired", async function () {
      const unwiredSettlement = await viem.deployContract("IntexSettlement", [
        deployer.account.address,
      ]);

      await viem.assertions.revertWithCustomError(
        unwiredSettlement.write.settle(
          [SERIES_ID, holder.account.address, 1n],
          { account: holder.account.address },
        ),
        unwiredSettlement,
        "NotWired",
      );
    });

  });

  describe("Admin Setters", async function () {
    it("Should revert wire when already wired", async function () {
      const { intex, usdc, vaultProvider, settlement, promis } = await deployContracts();

      await viem.assertions.revertWithCustomError(
        settlement.write.wire([intex.address, vaultProvider.address, usdc.address, promis.address]),
        settlement,
        "AlreadyWired",
      );
    });
  });

  describe("Partial Settlement", async function () {
    it("Should keep series Called and only burn `amount` Issued on partial settle", async function () {
      const { intex, promis, settlement, usdc } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);
      await fundAndApprove(usdc, settlement, holder.account.address, STRIKE_PRICE * 4n);

      await settlement.write.settle(
        [SERIES_ID, holder.account.address, 4n],
        { account: holder.account.address },
      );

      const sTok = await settledTokenId(intex);
      assert.equal(await intex.read.balanceOf([holder.account.address, TOKEN_ID]), 6n);
      assert.equal(await intex.read.balanceOf([holder.account.address, sTok]), 4n);
      assert.equal(await intex.read.totalSupply([TOKEN_ID]), 6n);
      assert.equal(await promis.read.balanceOf([holder.account.address]), 0n);

      const data = await intex.read.readData([SERIES_ID]);
      // 2 = Called (Issued=0, Qualified=1, Called=2)
      assert.equal(data.state, 2);
    });

    it("Should accumulate Settled across two equal partial settles", async function () {
      const { intex, settlement, usdc } = await deployContracts();
      await setupCalledSeries(intex, holder.account.address, 10n);
      await fundAndApprove(usdc, settlement, holder.account.address, STRIKE_PRICE * 10n);

      await settlement.write.settle(
        [SERIES_ID, holder.account.address, 5n],
        { account: holder.account.address },
      );
      await settlement.write.settle(
        [SERIES_ID, holder.account.address, 5n],
        { account: holder.account.address },
      );

      const sTok = await settledTokenId(intex);
      assert.equal(await intex.read.balanceOf([holder.account.address, sTok]), 10n);
      assert.equal(await intex.read.balanceOf([holder.account.address, TOKEN_ID]), 0n);

      const data = await intex.read.readData([SERIES_ID]);
      assert.equal(data.state, 2);
      assert.equal(await intex.read.totalSupply([TOKEN_ID]), 0n);
    });
  });
});
