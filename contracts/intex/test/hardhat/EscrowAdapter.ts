// We don't have Ethereum specific assertions in Hardhat 3 yet
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { network } from "hardhat";

describe("EscrowAdapter", async function () {
  const { viem } = await network.connect();

  // Test accounts - using hardhat's default accounts
  const [deployer, bridger, auction, _vaultEoaPlaceholder, bidder1, bidder2, outsider] = await viem.getWalletClients();
  // _vaultEoaPlaceholder kept to preserve account index ordering with prior version; the
  // actual vault is now a deployed `MockSettlementVault` wrapped by `MockVaultProvider`.
  void _vaultEoaPlaceholder;

  // Escrow state is keyed by uint32 seriesId.
  const seriesId1 = 1;
  const seriesId2 = 2;
  const LOCK_AMOUNT = 1000n * 10n ** 6n; // 1000 USDC (fits in uint64)

  // Stand-in for the inbound LZ packet GUID threaded into the finalization events.
  const GUID = ("0x" + "11".repeat(32)) as `0x${string}`;

  async function deployContracts() {
    // Deploy Mock ERC20 (payment token)
    const paymentToken = await viem.deployContract("MockERC20", ["USD Coin", "USDC", 6]);

    // Deploy Mock The Compact
    const compact = await viem.deployContract("MockTheCompact");

    // Deploy the underlying ERC4626-style vault and the outbe-vault provider that wraps it.
    const mockVault = await viem.deployContract("MockSettlementVault", [
      paymentToken.address,
      "Mock Vault USDC",
      "mvUSDC",
      6,
    ]);
    const vaultProvider = await viem.deployContract("MockVaultProvider", []);
    await vaultProvider.write.addVault([mockVault.address]);

    // Deploy EscrowAdapter
    const escrow = await viem.deployContract("EscrowAdapter", [
      deployer.account.address,
      bridger.account.address,
    ]);

    // Register escrow as a permitted depositor on the provider (production: outbe-vault
    // owner calls `addLiquiditySource(escrow, IntexBidPrice)` post-deploy). Slot 4 =
    // IntexBidPrice in the LiquiditySource enum.
    await vaultProvider.write.addLiquiditySource([escrow.address, 4]);

    // Wire dependencies (no allow-list precondition anymore).
    await escrow.write.wire(
      [auction.account.address, compact.address, vaultProvider.address, paymentToken.address],
      { account: deployer.account.address }
    );

    // Set reset period to 0 for immediate withdrawal in tests
    await compact.write.setResetPeriodSeconds([0n], { account: deployer.account.address });

    // Mint tokens to bidders
    await paymentToken.write.mint([bidder1.account.address, 10000n * 10n ** 6n]);
    await paymentToken.write.mint([bidder2.account.address, 10000n * 10n ** 6n]);

    // Approve escrow to spend bidder tokens
    await paymentToken.write.approve(
      [escrow.address, BigInt("0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")],
      { account: bidder1.account.address }
    );
    await paymentToken.write.approve(
      [escrow.address, BigInt("0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")],
      { account: bidder2.account.address }
    );

    return { escrow, compact, paymentToken, mockVault, vaultProvider };
  }

  describe("Contract Deployment and Setup", async function () {
    it("Should deploy with correct initial state", async function () {
      const { escrow } = await deployContracts();

      assert(await escrow.read.hasRole([await escrow.read.DEFAULT_ADMIN_ROLE(), deployer.account.address]));
      assert(await escrow.read.hasRole([await escrow.read.RELAYER_ROLE(), bridger.account.address]));
    });

    it("Should revert constructor with zero admin", async function () {
      await assert.rejects(
        viem.deployContract("EscrowAdapter", [
          "0x0000000000000000000000000000000000000000",
          bridger.account.address,
        ]),
        (err: unknown) => String(err).includes('ZeroAddress("defaultAdmin")'),
      );
    });

    it("Should revert constructor with zero bridger", async function () {
      await assert.rejects(
        viem.deployContract("EscrowAdapter", [
          deployer.account.address,
          "0x0000000000000000000000000000000000000000",
        ]),
        (err: unknown) => String(err).includes('ZeroAddress("bridger")'),
      );
    });

    it("Should wire dependencies correctly", async function () {
      const { escrow, compact, paymentToken, vaultProvider } = await deployContracts();

      assert.equal(
        (await escrow.read.intexAuctionContract()).toLowerCase(),
        auction.account.address.toLowerCase()
      );
      assert.equal(
        (await escrow.read.compact()).toLowerCase(),
        compact.address.toLowerCase()
      );
      assert.equal(
        (await escrow.read.vaultProvider()).toLowerCase(),
        vaultProvider.address.toLowerCase()
      );
      assert.equal(
        (await escrow.read.paymentToken()).toLowerCase(),
        paymentToken.address.toLowerCase()
      );
      assert(await escrow.read.hasRole([await escrow.read.AUCTION_ROLE(), auction.account.address]));
      assert(Number(await escrow.read.allocatorId()) > 0);
    });

    it("Should expose the payment-token class alias", async function () {
      const { escrow } = await deployContracts();
      assert.equal(Number(await escrow.read.PAYMENT_TOKEN_ALIAS()), 840);
    });

    it("Should revert wire with zero addresses", async function () {
      const escrow = await viem.deployContract("EscrowAdapter", [
        deployer.account.address,
        bridger.account.address,
      ]);

      const compact = await viem.deployContract("MockTheCompact");
      const paymentToken = await viem.deployContract("MockERC20", ["USD Coin", "USDC", 6]);
      const mockVault = await viem.deployContract("MockSettlementVault", [
        paymentToken.address,
        "Mock Vault USDC",
        "mvUSDC",
        6,
      ]);
      const vaultProvider = await viem.deployContract("MockVaultProvider", []);
      await vaultProvider.write.addVault([mockVault.address]);

      // Zero auction
      await viem.assertions.revertWithCustomError(
        escrow.write.wire(
          ["0x0000000000000000000000000000000000000000", compact.address, vaultProvider.address, paymentToken.address],
          { account: deployer.account.address }
        ),
        escrow,
        "ZeroAddress"
      );

      // Zero compact
      await viem.assertions.revertWithCustomError(
        escrow.write.wire(
          [auction.account.address, "0x0000000000000000000000000000000000000000", vaultProvider.address, paymentToken.address],
          { account: deployer.account.address }
        ),
        escrow,
        "ZeroAddress"
      );

      // Zero vault provider
      await viem.assertions.revertWithCustomError(
        escrow.write.wire(
          [auction.account.address, compact.address, "0x0000000000000000000000000000000000000000", paymentToken.address],
          { account: deployer.account.address }
        ),
        escrow,
        "ZeroAddress"
      );

      // Zero payment token
      await viem.assertions.revertWithCustomError(
        escrow.write.wire(
          [auction.account.address, compact.address, vaultProvider.address, "0x0000000000000000000000000000000000000000"],
          { account: deployer.account.address }
        ),
        escrow,
        "ZeroAddress"
      );
    });

    it("Should only allow admin to wire", async function () {
      const escrow = await viem.deployContract("EscrowAdapter", [
        deployer.account.address,
        bridger.account.address,
      ]);

      const compact = await viem.deployContract("MockTheCompact");
      const paymentToken = await viem.deployContract("MockERC20", ["USD Coin", "USDC", 6]);
      const mockVault = await viem.deployContract("MockSettlementVault", [
        paymentToken.address,
        "Mock Vault USDC",
        "mvUSDC",
        6,
      ]);
      const vaultProvider = await viem.deployContract("MockVaultProvider", []);
      await vaultProvider.write.addVault([mockVault.address]);

      await viem.assertions.revertWithCustomError(
        escrow.write.wire(
          [auction.account.address, compact.address, vaultProvider.address, paymentToken.address],
          { account: outsider.account.address }
        ),
        escrow,
        "AccessControlUnauthorizedAccount"
      );
    });
  });

  describe("Lock Funds", async function () {
    it("Should lock funds successfully", async function () {
      const { escrow, compact, paymentToken } = await deployContracts();

      const balanceBefore = await paymentToken.read.balanceOf([bidder1.account.address]);

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      // Check bidder balance decreased
      const balanceAfter = await paymentToken.read.balanceOf([bidder1.account.address]);
      assert.equal(balanceAfter, balanceBefore - LOCK_AMOUNT);

      // Check lock data
      const lock = await escrow.read.getBidLock([seriesId1, bidder1.account.address]);
      assert.equal(lock.lockedAmount, LOCK_AMOUNT);
      assert.equal(Number(lock.status), 1); // Locked
      assert(lock.lockedAt > 0);

      // Check auction stats
      const [hasLocks, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId1]);
      assert(hasLocks);
      assert(!isFinalized);
      assert.equal(totalLocked, LOCK_AMOUNT);
      assert.equal(
        await compact.read.balanceOf([escrow.address, await escrow.read.lockId()]),
        LOCK_AMOUNT,
      );
    });

    it("Should lock funds for multiple bidders", async function () {
      const { escrow, compact } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      await escrow.write.lockFunds([seriesId1, bidder2.account.address, LOCK_AMOUNT * 2n], {
        account: auction.account.address,
      });

      const [hasLocks, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId1]);
      assert(hasLocks);
      assert(!isFinalized);
      assert.equal(totalLocked, LOCK_AMOUNT * 3n);
      assert.equal(
        await compact.read.balanceOf([escrow.address, await escrow.read.lockId()]),
        LOCK_AMOUNT * 3n,
      );
    });

    it("Should revert lock with zero bidder", async function () {
      const { escrow } = await deployContracts();

      await viem.assertions.revertWithCustomError(
        escrow.write.lockFunds([
          seriesId1,
          "0x0000000000000000000000000000000000000000",
          LOCK_AMOUNT
        ], { account: auction.account.address }),
        escrow,
        "ZeroAddress"
      );
    });

    it("Should revert lock with zero amount", async function () {
      const { escrow } = await deployContracts();

      await viem.assertions.revertWithCustomError(
        escrow.write.lockFunds([seriesId1, bidder1.account.address, 0n], {
          account: auction.account.address,
        }),
        escrow,
        "ZeroValue"
      );
    });

    it("Should revert if already locked", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      await viem.assertions.revertWithCustomError(
        escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
          account: auction.account.address,
        }),
        escrow,
        "BidAlreadyLocked"
      );
    });

    it("Should only allow auction role to lock", async function () {
      const { escrow } = await deployContracts();

      await viem.assertions.revertWithCustomError(
        escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
          account: outsider.account.address,
        }),
        escrow,
        "AccessControlUnauthorizedAccount"
      );

      await viem.assertions.revertWithCustomError(
        escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
          account: deployer.account.address,
        }),
        escrow,
        "AccessControlUnauthorizedAccount"
      );
    });
  });

  describe("Finalize Auction", async function () {
    it("Should finalize with full refund", async function () {
      const { escrow, compact, paymentToken } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      const bidderBalanceBefore = await paymentToken.read.balanceOf([bidder1.account.address]);

      await escrow.write.finalizeAuction(
        [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
        { account: bridger.account.address }
      );

      // Check bidder received refund
      const bidderBalanceAfter = await paymentToken.read.balanceOf([bidder1.account.address]);
      assert.equal(bidderBalanceAfter, bidderBalanceBefore + LOCK_AMOUNT);

      // Check auction status
      const [hasLocks, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId1]);
      assert(hasLocks); // Count stays, but amount is 0
      assert(isFinalized);
      assert.equal(totalLocked, 0n);
      assert.equal(
        await compact.read.balanceOf([escrow.address, await escrow.read.lockId()]),
        0n,
      );

      // Check lock status
      const lock = await escrow.read.getBidLock([seriesId1, bidder1.account.address]);
      assert.equal(Number(lock.status), 2); // Finalized
    });

    it("Should finalize with full claim", async function () {
      const { escrow, paymentToken, mockVault } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      const vaultBalanceBefore = await paymentToken.read.balanceOf([mockVault.address]);

      await escrow.write.finalizeAuction(
        [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: 0n, paidAmount: LOCK_AMOUNT }]],
        { account: bridger.account.address }
      );

      // Check vault received funds (assets land in the underlying VaultV2 after provider routes them).
      const vaultBalanceAfter = await paymentToken.read.balanceOf([mockVault.address]);
      assert.equal(vaultBalanceAfter, vaultBalanceBefore + LOCK_AMOUNT);

      // Check accounting cleared
      const [, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId1]);
      assert(isFinalized);
      assert.equal(totalLocked, 0n);
    });

    it("Should finalize with partial refund and claim", async function () {
      const { escrow, paymentToken, mockVault } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      const bidderBalanceBefore = await paymentToken.read.balanceOf([bidder1.account.address]);
      const vaultBalanceBefore = await paymentToken.read.balanceOf([mockVault.address]);
      const refundedAmount = LOCK_AMOUNT * 30n / 100n; // 30% refund
      const paidAmount = LOCK_AMOUNT - refundedAmount; // 70% claim

      await escrow.write.finalizeAuction(
        [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount, paidAmount }]],
        { account: bridger.account.address }
      );

      // Check balances — refundedAmount + paidAmount must equal lockedAmount
      const bidderBalanceAfter = await paymentToken.read.balanceOf([bidder1.account.address]);
      const vaultBalanceAfter = await paymentToken.read.balanceOf([mockVault.address]);
      assert.equal(bidderBalanceAfter, bidderBalanceBefore + refundedAmount);
      assert.equal(vaultBalanceAfter, vaultBalanceBefore + paidAmount);
    });

    it("Should finalize multiple bidders", async function () {
      const { escrow, compact, paymentToken, mockVault } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });
      await escrow.write.lockFunds([seriesId1, bidder2.account.address, LOCK_AMOUNT * 2n], {
        account: auction.account.address,
      });

      const bidder1BalanceBefore = await paymentToken.read.balanceOf([bidder1.account.address]);
      const bidder2BalanceBefore = await paymentToken.read.balanceOf([bidder2.account.address]);
      const vaultBalanceBefore = await paymentToken.read.balanceOf([mockVault.address]);

      // bidder1 gets full refund; bidder2 gets a 50/50 split.
      await escrow.write.finalizeAuction(
        [
          seriesId1,
          GUID,
          [
            { bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n },
            { bidder: bidder2.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: LOCK_AMOUNT },
          ],
        ],
        { account: bridger.account.address }
      );

      // Verify per-bidder fund movement
      assert.equal(
        await paymentToken.read.balanceOf([bidder1.account.address]),
        bidder1BalanceBefore + LOCK_AMOUNT
      );
      assert.equal(
        await paymentToken.read.balanceOf([bidder2.account.address]),
        bidder2BalanceBefore + LOCK_AMOUNT
      );
      assert.equal(
        await paymentToken.read.balanceOf([mockVault.address]),
        vaultBalanceBefore + LOCK_AMOUNT
      );

      // All escrow drained for the series.
      const [, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId1]);
      assert(isFinalized);
      assert.equal(totalLocked, 0n);
      assert.equal(
        await compact.read.balanceOf([escrow.address, await escrow.read.lockId()]),
        0n,
      );
    });

    it("Should revert finalize with empty instructions", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      await viem.assertions.revertWithCustomError(
        escrow.write.finalizeAuction([seriesId1, GUID, []], { account: bridger.account.address }),
        escrow,
        "ZeroValue"
      );
    });

    it("Should revert if already finalized", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      await escrow.write.finalizeAuction(
        [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
        { account: bridger.account.address }
      );

      await viem.assertions.revertWithCustomError(
        escrow.write.finalizeAuction(
          [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
          { account: bridger.account.address }
        ),
        escrow,
        "AlreadyFinalized"
      );
    });

    it("Should revert finalize with zero bidder", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      // Per-bidder fail-safe: zero-address bidder fails inside try/catch; outer call succeeds
      // and the lock for an actual bidder (if any) stays recoverable. With no real lock here,
      // the call processes the single bad instruction and finalizes the series.
      await escrow.write.finalizeAuction(
        [seriesId1, GUID, [{ bidder: "0x0000000000000000000000000000000000000000", refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
        { account: bridger.account.address }
      );
    });

    it("Should finalize with fail-safe when lock not active (no outer revert)", async function () {
      const { escrow } = await deployContracts();

      // No lock created for the series — per-bidder try/catch catches LockNotActive
      // and emits BidderRefundFailed; outer call succeeds and series is marked finalized.
      await escrow.write.finalizeAuction(
        [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
        { account: bridger.account.address }
      );

      const [, finalized,] = await escrow.read.getAuctionStatus([seriesId1]);
      assert.equal(finalized, true);
    });

    it("Should finalize with fail-safe on amount mismatch (lock stays recoverable)", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      // refundedAmount + paidAmount != lockedAmount — caught by per-bidder try/catch.
      await escrow.write.finalizeAuction(
        [
          seriesId1,
          GUID,
          [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT / 2n, paidAmount: LOCK_AMOUNT / 2n - 1n }],
        ],
        { account: bridger.account.address }
      );

      // bidder1's lock remains active and recoverable via retryFinalize / claimRefund.
      const lock = await escrow.read.getBidLock([seriesId1, bidder1.account.address]);
      assert.equal(lock.status, 1, "lock should still be in Locked status (1)");
    });

    it("Should only allow bridge role to finalize", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      await viem.assertions.revertWithCustomError(
        escrow.write.finalizeAuction(
          [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
          { account: outsider.account.address }
        ),
        escrow,
        "AccessControlUnauthorizedAccount"
      );

      await viem.assertions.revertWithCustomError(
        escrow.write.finalizeAuction(
          [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
          { account: deployer.account.address }
        ),
        escrow,
        "AccessControlUnauthorizedAccount"
      );

      await viem.assertions.revertWithCustomError(
        escrow.write.finalizeAuction(
          [seriesId1, GUID, [{ bidder: bidder1.account.address, refundedAmount: LOCK_AMOUNT, paidAmount: 0n }]],
          { account: auction.account.address }
        ),
        escrow,
        "AccessControlUnauthorizedAccount"
      );
    });
  });

  describe("View Functions", async function () {
    it("Should return correct bid lock data", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      const lock = await escrow.read.getBidLock([seriesId1, bidder1.account.address]);
      assert.equal(lock.lockedAmount, LOCK_AMOUNT);
      assert.equal(Number(lock.status), 1); // Locked
      assert(lock.lockedAt > 0);
    });

    it("Should return empty lock for non-existent", async function () {
      const { escrow } = await deployContracts();

      const lock = await escrow.read.getBidLock([seriesId1, bidder1.account.address]);
      assert.equal(lock.lockedAmount, 0n);
      assert.equal(Number(lock.status), 0); // None
    });

    it("Should return correct auction status", async function () {
      const { escrow } = await deployContracts();

      // Before any locks
      let [hasLocks, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId1]);
      assert(!hasLocks);
      assert(!isFinalized);
      assert.equal(totalLocked, 0n);

      // After lock
      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      [hasLocks, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId1]);
      assert(hasLocks);
      assert(!isFinalized);
      assert.equal(totalLocked, LOCK_AMOUNT);
    });

    it("Should isolate state across series", async function () {
      const { escrow } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      // seriesId2 must remain untouched.
      const [hasLocks, isFinalized, totalLocked] = await escrow.read.getAuctionStatus([seriesId2]);
      assert(!hasLocks);
      assert(!isFinalized);
      assert.equal(totalLocked, 0n);
    });
  });

  describe("IAllocator Implementation", async function () {
    it("Should return selector for valid lock ID in attest", async function () {
      const { escrow } = await deployContracts();

      // First lock some funds to set lockId
      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      const lockId = await escrow.read.lockId();
      const result = await escrow.read.attest([
        "0x0000000000000000000000000000000000000000",
        "0x0000000000000000000000000000000000000000",
        "0x0000000000000000000000000000000000000000",
        lockId,
        0n,
      ]);
      // IAllocator.attest.selector = 0x1a808f91
      assert.equal(result, "0x1a808f91");
    });

    it("Should revert attest for invalid lock ID", async function () {
      const { escrow } = await deployContracts();

      // First lock some funds to set lockId
      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      await viem.assertions.revertWithCustomError(
        escrow.read.attest([
          "0x0000000000000000000000000000000000000000",
          "0x0000000000000000000000000000000000000000",
          "0x0000000000000000000000000000000000000000",
          999n,
          0n,
        ]),
        escrow,
        "UnexpectedLockId"
      );
    });

    it("Should always return false for isClaimAuthorized", async function () {
      const { escrow } = await deployContracts();

      const result = await escrow.read.isClaimAuthorized([
        "0x0000000000000000000000000000000000000000000000000000000000000000",
        "0x0000000000000000000000000000000000000000",
        "0x0000000000000000000000000000000000000000",
        0n,
        0n,
        [],
        "0x",
      ]);
      assert(!result);
    });
  });

  describe("Interface Support", async function () {
    it("Should support AccessControl interface", async function () {
      const { escrow } = await deployContracts();

      // AccessControl interface ID
      const supportsAccessControl = await escrow.read.supportsInterface(["0x7965db0b"]);
      assert(supportsAccessControl);
    });
  });

  describe("Payment Token Rotation", async function () {
    it("Should reject rotating payment token while locks are live", async function () {
      const { escrow, compact, vaultProvider } = await deployContracts();

      // Create a live lock with the current paymentToken.
      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      // Rewire targeting a new token while locks are still in flight — must revert.
      const usdt = await viem.deployContract("MockERC20", ["Tether", "USDT", 6]);
      await viem.assertions.revertWithCustomError(
        escrow.write.wire(
          [auction.account.address, compact.address, vaultProvider.address, usdt.address],
          { account: deployer.account.address }
        ),
        escrow,
        "LiveLocksOutstanding"
      );
    });

    it("Should allow rotating payment token when no locks are live", async function () {
      const { escrow, compact, vaultProvider } = await deployContracts();

      const usdt = await viem.deployContract("MockERC20", ["Tether", "USDT", 6]);
      await escrow.write.wire(
        [auction.account.address, compact.address, vaultProvider.address, usdt.address],
        { account: deployer.account.address }
      );
      assert.equal(
        (await escrow.read.paymentToken()).toLowerCase(),
        usdt.address.toLowerCase()
      );
    });

    it("Should allow re-wiring with same token while locks exist", async function () {
      const { escrow, compact, paymentToken } = await deployContracts();

      await escrow.write.lockFunds([seriesId1, bidder1.account.address, LOCK_AMOUNT], {
        account: auction.account.address,
      });

      // Same paymentToken, just swapping the vault provider — must succeed even with locks.
      const newMockVault = await viem.deployContract("MockSettlementVault", [
        paymentToken.address,
        "New Mock Vault USDC",
        "nmvUSDC",
        6,
      ]);
      const newProvider = await viem.deployContract("MockVaultProvider", []);
      await newProvider.write.addVault([newMockVault.address]);
      await escrow.write.wire(
        [auction.account.address, compact.address, newProvider.address, paymentToken.address],
        { account: deployer.account.address }
      );
      assert.equal(
        (await escrow.read.vaultProvider()).toLowerCase(),
        newProvider.address.toLowerCase()
      );
    });
  });
});
