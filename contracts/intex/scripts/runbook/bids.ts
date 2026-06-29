// EIP-712 bid signing matching IntexAuction._verifyRevealSignature.

import { keccak256, type Address, type Hex } from "viem";
import { privateKeyToAccount } from "viem/accounts";

async function signRevealBid(
  seriesId: number,
  bidderAddress: Address,
  quantity: bigint,
  bidRate: bigint,
  chainId: bigint,
  auctionAddress: Address,
  privateKey: Hex,
): Promise<Hex> {
  const account = privateKeyToAccount(privateKey);
  return account.signTypedData({
    domain: { name: "IntexAuction", version: "1", chainId: Number(chainId), verifyingContract: auctionAddress },
    types: {
      RevealBid: [
        { name: "seriesId", type: "uint32" },
        { name: "bidder", type: "address" },
        { name: "quantity", type: "uint16" },
        { name: "bidRate", type: "uint32" },
      ],
    },
    primaryType: "RevealBid",
    message: { seriesId, bidder: bidderAddress, quantity: Number(quantity), bidRate: Number(bidRate) },
  });
}

export async function createCommitHash(
  seriesId: number,
  bidderAddress: Address,
  quantity: bigint,
  bidRate: bigint,
  chainId: bigint,
  auctionAddress: Address,
  privateKey: Hex,
): Promise<Hex> {
  return keccak256(await signRevealBid(seriesId, bidderAddress, quantity, bidRate, chainId, auctionAddress, privateKey));
}

export async function createRevealSignature(
  seriesId: number,
  bidderAddress: Address,
  quantity: bigint,
  bidRate: bigint,
  chainId: bigint,
  auctionAddress: Address,
  privateKey: Hex,
): Promise<Hex> {
  return signRevealBid(seriesId, bidderAddress, quantity, bidRate, chainId, auctionAddress, privateKey);
}
