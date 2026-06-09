/**
 * BridgeMsgCodec encode/decode roundtrip tests.
 *
 * Verifies that encodePacked messages with sub-32-byte types (uint32, uint64)
 * are decoded at the correct byte offsets.
 */
import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { network } from "hardhat";

describe("BridgeMsgCodec – encode/decode roundtrip", async function () {
  const { viem } = await network.connect();

  const codec = await viem.deployContract("CodecHarness");

  const SERIES_ID = 20250115; // yyyymmdd as uint32

  // -------------------------------------------------------------------
  // decodeAuctionResult
  //   encodePacked layout (22 bytes):
  //   [bodyVersion(1)][msgType(1)][seriesId(4)][issuedIntexCount(4)][auctionIntexClearingPrice(8)][wonBidsCount(4)]
  // -------------------------------------------------------------------
  it("decodeAuctionResult – roundtrip preserves values", async function () {
    const issuedIntexCount = 500n;
    const clearingPrice = 75_000_000n; // 75e6
    const wonBidsCount = 42n;

    const encoded = await codec.read.encodeAuctionResult([
      SERIES_ID,
      issuedIntexCount,
      clearingPrice,
      wonBidsCount,
    ]);

    // encodePacked: 1 + 1 + 4 + 4 + 8 + 4 = 22 bytes
    const byteLen = (encoded.length - 2) / 2; // subtract "0x", hex chars / 2
    assert.equal(byteLen, 22, `encoded length should be 22, got ${byteLen}`);

    const [decodedSeriesId, decodedCount, decodedPrice, decodedWonBidsCount] =
      await codec.read.decodeAuctionResult([encoded]);

    assert.equal(Number(decodedSeriesId), SERIES_ID, "seriesId mismatch");
    assert.equal(Number(decodedCount), Number(issuedIntexCount), "issuedIntexCount mismatch");
    assert.equal(decodedPrice, clearingPrice, "clearingPrice mismatch");
    assert.equal(Number(decodedWonBidsCount), Number(wonBidsCount), "wonBidsCount mismatch");
  });

  // -------------------------------------------------------------------
  // decodeMarkCalled
  //   encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
  // -------------------------------------------------------------------
  it("decodeMarkCalled – roundtrip preserves values", async function () {
    const encoded = await codec.read.encodeMarkCalled([SERIES_ID]);

    // encodePacked: 1 + 1 + 4 = 6 bytes
    const byteLen = (encoded.length - 2) / 2;
    assert.equal(byteLen, 6, `encoded length should be 6, got ${byteLen}`);

    const decodedSeriesId = await codec.read.decodeMarkCalled([encoded]);

    assert.equal(Number(decodedSeriesId), SERIES_ID, "seriesId mismatch");
  });

  // -------------------------------------------------------------------
  // decodeMarkQualified
  //   encodePacked layout (6 bytes): [bodyVersion(1)][msgType(1)][seriesId(4)]
  // -------------------------------------------------------------------
  it("decodeMarkQualified – roundtrip preserves values", async function () {
    const encoded = await codec.read.encodeMarkQualified([SERIES_ID]);

    // encodePacked: 1 + 1 + 4 = 6 bytes
    const byteLen = (encoded.length - 2) / 2;
    assert.equal(byteLen, 6, `encoded length should be 6, got ${byteLen}`);

    const decodedSeriesId = await codec.read.decodeMarkQualified([encoded]);

    assert.equal(Number(decodedSeriesId), SERIES_ID, "seriesId mismatch");
  });
});
