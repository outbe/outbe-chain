import { type Hex, bytesToHex, toBytes } from "viem";
import type { Ctx } from "../chain.js";
import type { EncryptedOffer } from "../crypto.js";
import { OFFERING_STATUS, resolveContract } from "../registry.js";

/** The DKG-derived key offers are encrypted to. Fails until the TeeRegistry is bootstrapped. */
export async function offerPublicKey(ctx: Ctx): Promise<Uint8Array> {
  const tee = resolveContract("teeregistry");
  const bootstrapped = await ctx.publicClient.readContract({
    address: tee.address,
    abi: tee.abi,
    functionName: "isBootstrapped",
  });
  if (!bootstrapped) throw new Error("TeeRegistry not bootstrapped - no offer key yet");
  const offerKeyU256 = (await ctx.publicClient.readContract({
    address: tee.address,
    abi: tee.abi,
    functionName: "tributeOfferPublicKey",
  })) as bigint;
  return toBytes(offerKeyU256, { size: 32 });
}

/** The first WorldwideDay in OFFERING status. */
export async function offeringDay(ctx: Ctx): Promise<number> {
  const md = resolveContract("metadosis");
  const offering = (await ctx.publicClient.readContract({
    address: md.address,
    abi: md.abi,
    functionName: "getWorldwideDaysByStatus",
    args: [OFFERING_STATUS],
  })) as readonly number[];
  if (!offering.length) throw new Error("no WorldwideDay is currently in OFFERING status");
  return Number(offering[0]);
}

export interface OfferTerms {
  worldwideDay: number;
  currency: number;
  excludeFromIntex: boolean;
  zkProof: Hex;
  l2ChainId: number;
  circuitVersion: string;
  zkMerkleRoot: Hex;
  signature: Hex;
}

/** `offerTribute` arguments in ABI order. */
export function offerTributeArgs(enc: EncryptedOffer, terms: OfferTerms): unknown[] {
  return [
    bytesToHex(enc.cipherText),
    bytesToHex(enc.nonce),
    enc.ephemeralPubkey,
    terms.worldwideDay,
    terms.currency, // tributeCurrency
    terms.currency, // referenceCurrency - a separate axis, same value here
    terms.excludeFromIntex,
    terms.zkProof,
    terms.l2ChainId, // chainId the proof verifies under
    terms.circuitVersion, // exact enabled circuit version for that chain
    "0x" as Hex, // zkPublicKey - the combined proof carries its own
    terms.zkMerkleRoot,
    terms.signature,
  ];
}
