// We don't have Ethereum specific assertions in Hardhat 3 yet
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { network } from "hardhat";
import { keccak256, type Hex } from "viem";
import { privateKeyToAccount } from "viem/accounts";

describe("IntexAuction", async function () {
  const { viem, networkHelpers } = await network.connect();

  // Test accounts - using hardhat's default accounts
  const [deployer, bridger, user1, user2, user3] = await viem.getWalletClients();

  // Private key for user1 (Hardhat default third account -> 0x3C44cddD...)
  const user1PrivateKey = "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a" as Hex;

  const chainId = 31337n; // Hardhat default chain ID; also passed to revealBid as the
  //                          `chainId == block.chainid` belt-and-braces param.

  // Schedule offsets relative to the auction-start timestamp.
  const COMMIT_OFFSET = 100;
  const REVEAL_OFFSET = 200;
  const ISSUANCE_OFFSET = 300;

  // Helper: build an EIP-712 reveal signature against the given IntexAuction instance.
  // Mirrors `IntexAuction.REVEAL_BID_TYPEHASH` and the `EIP712("IntexAuction", "1")` domain.
  // The commit hash remains `keccak256(signature)`.
  async function signBid(
    seriesId: number,
    bidder: string,
    quantity: bigint,
    bidPrice: bigint,
    verifyingContract: `0x${string}`,
    privateKey: Hex,
  ): Promise<Hex> {
    const account = privateKeyToAccount(privateKey);
    return account.signTypedData({
      domain: {
        name: "IntexAuction",
        version: "1",
        chainId: Number(chainId),
        verifyingContract,
      },
      types: {
        RevealBid: [
          { name: "seriesId", type: "uint32" },
          { name: "bidder", type: "address" },
          { name: "quantity", type: "uint16" },
          { name: "bidPrice", type: "uint64" },
        ],
      },
      primaryType: "RevealBid",
      message: {
        seriesId,
        bidder: bidder as `0x${string}`,
        quantity: Number(quantity),
        bidPrice,
      },
    });
  }

  async function deployContracts() {
    // Deploy Mock Escrow Adapter
    const escrowAdapter = await viem.deployContract("MockEscrowAdapter");

    // Deploy IntexAuction contract
    const auction = await viem.deployContract("IntexAuction", [
      deployer.account.address,
      bridger.account.address,
    ]);

    // Setup roles and wire
    await auction.write.grantRole(
      [await auction.read.RELAYER_ROLE(), bridger.account.address],
      { account: deployer.account.address },
    );

    await auction.write.wire([escrowAdapter.address], {
      account: deployer.account.address,
    });

    return { auction, escrowAdapter };
  }

  // Build a valid, strictly-increasing, in-the-future schedule anchored to the
  // current block timestamp. Returns the schedule struct plus the anchor timestamp.
  async function buildSchedule() {
    const now = await networkHelpers.time.latest();
    const schedule = {
      commitEnd: now + COMMIT_OFFSET,
      revealEnd: now + REVEAL_OFFSET,
      issuanceEnd: now + ISSUANCE_OFFSET,
    };
    return { schedule, anchor: now };
  }

  // Auction params struct with the given minimum bid price, intex size and min quantity.
  function buildParams(minIntexBidPrice: bigint, intexSize: bigint, minIntexBidQuantity: number) {
    return {
      intexSize,
      minIntexBidPrice,
      intexStrikePrice: 100n,
      coenPriceFloor: 100n,
      minIntexBidQuantity,
    };
  }

  // Create and start an auction as the relayer. Returns the schedule + anchor timestamp.
  async function startAuction(
    auction: Awaited<ReturnType<typeof deployContracts>>["auction"],
    seriesId: number,
    minIntexBidPrice: bigint,
    intexSize: bigint,
    minIntexBidQuantity: number,
  ) {
    const { schedule, anchor } = await buildSchedule();
    await auction.write.auctionStart(
      [seriesId, schedule, buildParams(minIntexBidPrice, intexSize, minIntexBidQuantity)],
      { account: bridger.account.address },
    );
    return { schedule, anchor };
  }

  // Send the green-day signal and advance time past commitEnd so the computed
  // stage is actually RevealingBids (stage is derived from schedule + day state).
  async function enterRevealStage(
    auction: Awaited<ReturnType<typeof deployContracts>>["auction"],
    seriesId: number,
    anchor: number,
  ) {
    await auction.write.startRevealingBidsStage([seriesId, true], {
      account: bridger.account.address,
    });
    await networkHelpers.time.increaseTo(anchor + COMMIT_OFFSET + 1);
  }

  describe("Contract Deployment and Setup", async function () {
    it("Should deploy with correct initial state", async function () {
      const { auction } = await deployContracts();

      // Check roles
      assert(await auction.read.hasRole([await auction.read.DEFAULT_ADMIN_ROLE(), deployer.account.address]));
      assert(await auction.read.hasRole([await auction.read.RELAYER_ROLE(), bridger.account.address]));
    });

    it("Should wire dependencies correctly", async function () {
      const { auction, escrowAdapter } = await deployContracts();

      assert.equal(
        (await auction.read.escrowContract()).toLowerCase(),
        escrowAdapter.address.toLowerCase(),
      );
    });

    it("Should reject wire with zero escrow address", async function () {
      const { auction } = await deployContracts();

      await viem.assertions.revertWithCustomError(
        auction.write.wire(["0x0000000000000000000000000000000000000000"], {
          account: deployer.account.address,
        }),
        auction,
        "ZeroAddress",
      );
    });
  });

  describe("IntexAuction Lifecycle", async function () {
    it("Should create auction successfully", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250115; // yyyymmdd format
      const minIntexBidPrice = 50n * 10n ** 6n; // 50 with 6 decimals
      const intexSize = 1000n;
      const minIntexBidQuantity = 1;

      const { schedule } = await startAuction(auction, seriesId, minIntexBidPrice, intexSize, minIntexBidQuantity);

      const auctionData = await auction.read.getAuctionInfo([seriesId]);
      assert.equal(Number(auctionData.worldwideDayState), 0); // Unknown
      assert.equal(Number(auctionData.schedule.commitEnd), schedule.commitEnd);
      assert.equal(Number(auctionData.schedule.revealEnd), schedule.revealEnd);
      assert.equal(Number(auctionData.schedule.issuanceEnd), schedule.issuanceEnd);
      assert.equal(auctionData.params.minIntexBidPrice, minIntexBidPrice);
      assert.equal(auctionData.params.intexSize, intexSize);
      assert.equal(Number(auctionData.params.minIntexBidQuantity), minIntexBidQuantity);

      const stage = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stage), 0); // CommittingBids
    });

    it("Should reject auction creation from unauthorized user", async function () {
      const { auction } = await deployContracts();

      const { schedule } = await buildSchedule();

      await viem.assertions.revertWithCustomError(
        auction.write.auctionStart([20250116, schedule, buildParams(50n * 10n ** 6n, 1000n, 1)], {
          account: user1.account.address,
        }),
        auction,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should reject auction creation with an invalid schedule", async function () {
      const { auction } = await deployContracts();

      const now = await networkHelpers.time.latest();

      // commitEnd in the past.
      await viem.assertions.revertWithCustomError(
        auction.write.auctionStart(
          [20250117, { commitEnd: now, revealEnd: now + 200, issuanceEnd: now + 300 }, buildParams(1n, 1000n, 1)],
          { account: bridger.account.address },
        ),
        auction,
        "InvalidSchedule",
      );

      // Schedule not strictly increasing (revealEnd <= commitEnd).
      await viem.assertions.revertWithCustomError(
        auction.write.auctionStart(
          [
            20250117,
            { commitEnd: now + 200, revealEnd: now + 200, issuanceEnd: now + 300 },
            buildParams(1n, 1000n, 1),
          ],
          { account: bridger.account.address },
        ),
        auction,
        "InvalidSchedule",
      );
    });

    it("Should reject creating an auction that already exists", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250118;
      await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const { schedule } = await buildSchedule();
      await viem.assertions.revertWithCustomError(
        auction.write.auctionStart([seriesId, schedule, buildParams(50n * 10n ** 6n, 1000n, 1)], {
          account: bridger.account.address,
        }),
        auction,
        "AuctionAlreadyExists",
      );
    });
  });

  describe("Commit Phase", async function () {
    it("Should allow valid bid commitment", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250119;
      await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const quantity = 10n;
      const bidPrice = 80n * 10n ** 6n;
      const signature = await signBid(seriesId, user1.account.address, quantity, bidPrice, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);

      await auction.write.commitBid([seriesId, commitHash], {
        account: user1.account.address,
      });

      const committedHash = await auction.read.committedBidsByHash([seriesId, user1.account.address]);
      assert.equal(committedHash, commitHash);

      const [committedBidsCount] = await auction.read.auctionRunningCounts([seriesId]);
      assert.equal(Number(committedBidsCount), 1);
    });

    it("Should allow cancelling commitment", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250120;
      await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const signature = await signBid(seriesId, user1.account.address, 10n, 80n * 10n ** 6n, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);

      await auction.write.commitBid([seriesId, commitHash], {
        account: user1.account.address,
      });

      await auction.write.cancelCommit([seriesId], {
        account: user1.account.address,
      });

      const committedHash = await auction.read.committedBidsByHash([seriesId, user1.account.address]);
      assert.equal(committedHash, "0x0000000000000000000000000000000000000000000000000000000000000000");

      const [committedBidsCount] = await auction.read.auctionRunningCounts([seriesId]);
      assert.equal(Number(committedBidsCount), 0);
    });

    it("Should reject cancelling when no commitment exists", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250121;
      await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      await viem.assertions.revertWithCustomError(
        auction.write.cancelCommit([seriesId], { account: user1.account.address }),
        auction,
        "BidNotFound",
      );
    });

    it("Should reject commit past commitEnd while the green-day signal is still Unknown", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250122;
      const { anchor } = await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const signature = await signBid(seriesId, user1.account.address, 10n, 80n * 10n ** 6n, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);

      // No green-day signal: the derived stage stays CommittingBids past commitEnd, but the
      // explicit deadline gate must reject.
      await networkHelpers.time.increaseTo(anchor + COMMIT_OFFSET + 10);

      await viem.assertions.revertWithCustomError(
        auction.write.commitBid([seriesId, commitHash], { account: user1.account.address }),
        auction,
        "CommitWindowClosed",
      );
    });

    it("Should reject cancel past commitEnd while the green-day signal is still Unknown", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250123;
      const { anchor } = await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const signature = await signBid(seriesId, user1.account.address, 10n, 80n * 10n ** 6n, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);
      await auction.write.commitBid([seriesId, commitHash], { account: user1.account.address });

      // A sealed commit must not be withdrawable after commitEnd.
      await networkHelpers.time.increaseTo(anchor + COMMIT_OFFSET + 10);

      await viem.assertions.revertWithCustomError(
        auction.write.cancelCommit([seriesId], { account: user1.account.address }),
        auction,
        "CommitWindowClosed",
      );

      // The commit survives the rejected cancel.
      const committedHash = await auction.read.committedBidsByHash([seriesId, user1.account.address]);
      assert.equal(committedHash, commitHash);
    });
  });

  describe("Reveal Phase", async function () {
    it("Should allow valid bid reveal", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250122;
      const { anchor } = await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const quantity = 10n;
      const bidPrice = 80n * 10n ** 6n;

      // Create signature once and reuse for commit and reveal
      const signature = await signBid(seriesId, user1.account.address, quantity, bidPrice, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);

      // Commit
      await auction.write.commitBid([seriesId, commitHash], {
        account: user1.account.address,
      });

      // Move to reveal stage (green day + advance past commitEnd)
      await enterRevealStage(auction, seriesId, anchor);

      // Verify signature hash matches commit hash before reveal
      assert.equal(keccak256(signature), commitHash, "Signature hash must match commit hash");

      // Reveal using the same signature
      await auction.write.revealBid([seriesId, quantity, bidPrice, chainId, signature], {
        account: user1.account.address,
      });

      const revealed = await auction.read.revealedBidsByBidder([seriesId, user1.account.address]);
      assert.equal(revealed, true);

      const [, revealedBidsCount] = await auction.read.auctionRunningCounts([seriesId]);
      assert.equal(Number(revealedBidsCount), 1);

      const [, bids] = await auction.read.getAuctionDetails([seriesId]);
      assert.equal(bids.length, 1);
    });

    it("Should reject reveal with invalid signature", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250123;
      const { anchor } = await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const quantity = 10n;
      const bidPrice = 80n * 10n ** 6n;

      // Commit with valid signature
      const validSignature = await signBid(seriesId, user1.account.address, quantity, bidPrice, auction.address, user1PrivateKey);
      const commitHash = keccak256(validSignature);
      await auction.write.commitBid([seriesId, commitHash], {
        account: user1.account.address,
      });

      // Move to reveal stage
      await enterRevealStage(auction, seriesId, anchor);

      // Try to reveal with wrong price (different signature)
      const wrongSignature = await signBid(
        seriesId,
        user1.account.address,
        quantity,
        bidPrice + 10n * 10n ** 6n,
        auction.address,
        user1PrivateKey,
      );

      await viem.assertions.revertWithCustomError(
        auction.write.revealBid([seriesId, quantity, bidPrice, chainId, wrongSignature], {
          account: user1.account.address,
        }),
        auction,
        "RevealHashMismatch",
      );
    });

    it("Should reject reveal below floor price", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250124;
      const minIntexBidPrice = 100n * 10n ** 6n;
      const { anchor } = await startAuction(auction, seriesId, minIntexBidPrice, 1000n, 1);

      const quantity = 10n;
      const bidPrice = 90n * 10n ** 6n; // Below floor

      // Commit with signature below floor price
      const signature = await signBid(seriesId, user1.account.address, quantity, bidPrice, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);
      await auction.write.commitBid([seriesId, commitHash], {
        account: user1.account.address,
      });

      // Move to reveal stage
      await enterRevealStage(auction, seriesId, anchor);

      await viem.assertions.revertWithCustomError(
        auction.write.revealBid([seriesId, quantity, bidPrice, chainId, signature], {
          account: user1.account.address,
        }),
        auction,
        "BidBelowMinIntexBidPrice",
      );
    });

    it("Should reject reveal without a commitment", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250125;
      const { anchor } = await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      await enterRevealStage(auction, seriesId, anchor);

      const quantity = 10n;
      const bidPrice = 80n * 10n ** 6n;
      const signature = await signBid(seriesId, user1.account.address, quantity, bidPrice, auction.address, user1PrivateKey);

      await viem.assertions.revertWithCustomError(
        auction.write.revealBid([seriesId, quantity, bidPrice, chainId, signature], {
          account: user1.account.address,
        }),
        auction,
        "BidNotFound",
      );
    });

    it("Should reject a double reveal", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250126;
      const { anchor } = await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      const quantity = 10n;
      const bidPrice = 80n * 10n ** 6n;
      const signature = await signBid(seriesId, user1.account.address, quantity, bidPrice, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);

      await auction.write.commitBid([seriesId, commitHash], {
        account: user1.account.address,
      });

      await enterRevealStage(auction, seriesId, anchor);

      await auction.write.revealBid([seriesId, quantity, bidPrice, chainId, signature], {
        account: user1.account.address,
      });

      await viem.assertions.revertWithCustomError(
        auction.write.revealBid([seriesId, quantity, bidPrice, chainId, signature], {
          account: user1.account.address,
        }),
        auction,
        "BidAlreadyRevealed",
      );
    });
  });

  describe("IntexAuction Clearing", async function () {
    it("Should execute auction clearing successfully", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250127;
      const intexSize = 1000n;
      const { anchor } = await startAuction(auction, seriesId, 50n * 10n ** 6n, intexSize, 1);

      const quantity = 10n;
      const bidPrice = 80n * 10n ** 6n;

      // Create signature once and reuse for commit and reveal
      const signature = await signBid(seriesId, user1.account.address, quantity, bidPrice, auction.address, user1PrivateKey);
      const commitHash = keccak256(signature);

      // Commit
      await auction.write.commitBid([seriesId, commitHash], {
        account: user1.account.address,
      });

      // Move to reveal stage
      await enterRevealStage(auction, seriesId, anchor);

      // Reveal using the same signature
      await auction.write.revealBid([seriesId, quantity, bidPrice, chainId, signature], {
        account: user1.account.address,
      });

      // The Issuance stage is time-derived (now >= revealEnd) once green-day is set;
      // startClearingStage is the bridge signal and does not itself change the stage.
      await networkHelpers.time.increaseTo(anchor + REVEAL_OFFSET + 1);
      await auction.write.startClearingStage([seriesId], {
        account: bridger.account.address,
      });

      const stageBeforeClearing = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stageBeforeClearing), 2); // Issuance

      // Execute clearing
      const issuedIntexCount = 100n;
      const auctionIntexClearingPrice = 75n * 10n ** 6n;
      const wonBidsCount = 1n;

      await auction.write.executeAuctionClearing(
        [seriesId, issuedIntexCount, auctionIntexClearingPrice, wonBidsCount],
        { account: bridger.account.address },
      );

      const auctionData = await auction.read.getAuctionInfo([seriesId]);
      assert.equal(auctionData.result.auctionIntexClearingPrice, auctionIntexClearingPrice);
      assert.equal(Number(auctionData.result.issuedIntexCount), Number(issuedIntexCount));
      assert.equal(Number(auctionData.result.wonBidsCount), Number(wonBidsCount));
      assert.equal(auctionData.params.intexSize, intexSize);
      // issuedIntexLoadedPromis is derived on-chain as issuedIntexCount * intexSize.
      assert.equal(auctionData.result.issuedIntexLoadedPromis, issuedIntexCount * intexSize);

      const stage = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stage), 3); // Completed
    });

    it("Should reject clearing with zero clearing price", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250128;
      const { anchor } = await startAuction(auction, seriesId, 10n, 1000n, 1);
      await enterRevealStage(auction, seriesId, anchor);

      // Reach the Issuance stage (time-derived: now >= revealEnd).
      await networkHelpers.time.increaseTo(anchor + REVEAL_OFFSET + 1);
      await auction.write.startClearingStage([seriesId], {
        account: bridger.account.address,
      });

      await viem.assertions.revertWithCustomError(
        auction.write.executeAuctionClearing([seriesId, 100n, 0n, 1n], {
          account: bridger.account.address,
        }),
        auction,
        "ZeroValue",
      );
    });

    it("Should handle red day (auction cancellation)", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250129;
      await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      // Start revealing stage with red day
      await auction.write.startRevealingBidsStage([seriesId, false], {
        account: bridger.account.address,
      });

      const stage = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stage), 4); // Cancelled

      const auctionData = await auction.read.getAuctionInfo([seriesId]);
      assert.equal(Number(auctionData.worldwideDayState), 2); // Red

      // Any action should now fail the stage requirement.
      const signature = await signBid(seriesId, user1.account.address, 1n, 50n * 10n ** 6n, auction.address, user1PrivateKey);
      await viem.assertions.revertWithCustomError(
        auction.write.commitBid([seriesId, keccak256(signature)], {
          account: user1.account.address,
        }),
        auction,
        "StageRequired",
      );
    });
  });

  describe("Access Control", async function () {
    it("Should reject startRevealingBidsStage from unauthorized user", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250130;
      await startAuction(auction, seriesId, 10n, 1000n, 1);

      await viem.assertions.revertWithCustomError(
        auction.write.startRevealingBidsStage([seriesId, true], {
          account: user1.account.address,
        }),
        auction,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should reject startClearingStage from unauthorized user", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250131;
      const { anchor } = await startAuction(auction, seriesId, 10n, 1000n, 1);
      await enterRevealStage(auction, seriesId, anchor);

      await viem.assertions.revertWithCustomError(
        auction.write.startClearingStage([seriesId], {
          account: user1.account.address,
        }),
        auction,
        "AccessControlUnauthorizedAccount",
      );
    });

    it("Should reject executeAuctionClearing from unauthorized user", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250201;
      const { anchor } = await startAuction(auction, seriesId, 10n, 1000n, 1);
      await enterRevealStage(auction, seriesId, anchor);
      await networkHelpers.time.increaseTo(anchor + REVEAL_OFFSET + 1);
      await auction.write.startClearingStage([seriesId], {
        account: bridger.account.address,
      });

      await viem.assertions.revertWithCustomError(
        auction.write.executeAuctionClearing([seriesId, 100n, 75n, 1n], {
          account: user1.account.address,
        }),
        auction,
        "AccessControlUnauthorizedAccount",
      );
    });
  });

  describe("View Functions", async function () {
    it("Should return correct auction details", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250202; // yyyymmdd format
      const minIntexBidPrice = 50n * 10n ** 6n;
      const intexSize = 1000n;

      const { schedule } = await startAuction(auction, seriesId, minIntexBidPrice, intexSize, 1);

      // getAuctionInfo
      const auctionData = await auction.read.getAuctionInfo([seriesId]);
      assert.equal(Number(auctionData.worldwideDayState), 0); // Unknown
      assert.equal(Number(auctionData.schedule.commitEnd), schedule.commitEnd);
      assert.equal(auctionData.params.minIntexBidPrice, minIntexBidPrice);

      // getAuctionDetails
      const [details, bids] = await auction.read.getAuctionDetails([seriesId]);
      assert.equal(details.params.intexSize, intexSize);
      assert.equal(bids.length, 0);
    });

    it("Should revert views for a non-existent auction", async function () {
      const { auction } = await deployContracts();

      const nonExistentSeries = 99999999;

      await viem.assertions.revertWithCustomError(
        auction.read.getAuctionInfo([nonExistentSeries]),
        auction,
        "AuctionNotFound",
      );
      await viem.assertions.revertWithCustomError(
        auction.read.getAuctionDetails([nonExistentSeries]),
        auction,
        "AuctionNotFound",
      );
      await viem.assertions.revertWithCustomError(
        auction.read.getAuctionStage([nonExistentSeries]),
        auction,
        "AuctionNotFound",
      );
    });

    it("Should return correct auction stage across timing transitions", async function () {
      const { auction } = await deployContracts();

      const seriesId = 20250203;
      const { schedule } = await startAuction(auction, seriesId, 50n * 10n ** 6n, 1000n, 1);

      let stage = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stage), 0); // CommittingBids

      // Time-based transitions only take effect once the green-day signal is received.
      await auction.write.startRevealingBidsStage([seriesId, true], {
        account: bridger.account.address,
      });

      // Early green-day signal snaps commitEnd to block.timestamp; stage transitions to
      // RevealingBids immediately — independent of the original schedule.commitEnd.
      stage = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stage), 1); // RevealingBids

      // Original commitEnd is now in the past; stage stays at RevealingBids.
      await networkHelpers.time.increaseTo(schedule.commitEnd + 1);
      stage = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stage), 1); // RevealingBids

      // After revealEnd -> Issuance.
      await networkHelpers.time.increaseTo(schedule.revealEnd + 1);
      stage = await auction.read.getAuctionStage([seriesId]);
      assert.equal(Number(stage), 2); // Issuance
    });
  });
});
