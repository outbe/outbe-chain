import { type Address, type Hex, keccak256 } from "viem";

/**
 * Deterministic commit/reveal binding for IntexAuction bids.
 *
 * There is no separate salt. The commit hash is `keccak256(signature)` where
 * `signature` is the EIP-712 RevealBid signature. ECDSA signatures are
 * deterministic (RFC 6979), so re-signing the same (key, series, qty, price) at
 * reveal reproduces the identical signature — nothing is stored between commit
 * and reveal, and it works across sessions and machines.
 *
 * Scheme (verbatim from contracts/intex/contracts/bnb/IntexAuction.sol):
 *  - domain  EIP712("IntexAuction", "1"), chainId = target chain, verifyingContract = auction
 *  - type    RevealBid(uint32 seriesId,address bidder,uint16 quantity,uint64 bidPrice)
 *  - commit  commitHash = keccak256(signature)
 *  - reveal  recovered signer must equal the bidder and keccak256(signature) the commit
 */

export interface RevealBidParams {
  chainId: number;
  verifyingContract: Address;
  seriesId: number;
  bidder: Address;
  quantity: number;
  bidPrice: bigint;
}

/** The EIP-712 typed-data object for a RevealBid, for viem `signTypedData`. */
export function revealBidTypedData(p: RevealBidParams) {
  return {
    domain: {
      name: "IntexAuction",
      version: "1",
      chainId: p.chainId,
      verifyingContract: p.verifyingContract,
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
      seriesId: p.seriesId,
      bidder: p.bidder,
      quantity: p.quantity,
      bidPrice: p.bidPrice,
    },
  } as const;
}

/** Commit hash = keccak256 of the EIP-712 reveal signature. */
export function commitHash(signature: Hex): Hex {
  return keccak256(signature);
}
