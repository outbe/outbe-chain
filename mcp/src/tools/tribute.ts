import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Hex, bytesToHex } from "viem";
import { z } from "zod";
import { type Ctx, sendTx } from "../chain.js";
import { buildPayload, canonicalAmountBase, canonicalAmountMicro, encryptOffer } from "../crypto.js";
import { RECEIPT_TIMEOUT_MS } from "../net/tx.js";
import { resolveContract } from "../registry.js";
import { offerPublicKey, offerTributeArgs, offeringDay } from "../tribute/offer.js";
import { HEX32, waitFlag } from "./schemas.js";
import { handler, ok, view } from "./util.js";

const GAS_OFFER = 8_000_000n;

const tributeBase = z
  .string()
  .refine((value) => {
    try {
      canonicalAmountBase(value);
      return true;
    } catch {
      return false;
    }
  }, "amount must be a canonical unsigned u64")
  .describe("whole unsigned COEN amount, canonical u64 string");

/** `0x`-hex of any width. The precompile decides whether the bytes themselves are valid. */
const HEX = /^0x([0-9a-fA-F]{2})*$/;

const zkProof = z
  .string()
  .regex(HEX, "zk_proof must be 0x-prefixed hex")
  .describe(
    "combined Tribute proof bytes (0x-hex): 4-byte public-input word count, public inputs, then proof",
  );
const zkMerkleRoot = z
  .string()
  .regex(HEX, "zk_merkle_root must be 0x-prefixed hex")
  .describe(
    "L2 Merkle root the proof's public input commits to; also checked against the L2 BLS signature",
  );
const zkSignature = z
  .string()
  .regex(HEX, "signature must be 0x-prefixed hex")
  .describe(
    "BLS MinSig signature (compressed G1, 48 bytes) over zk_merkle_root by the network key registered in the L2Registry",
  );
const circuitChainId = z
  .number()
  .int()
  .min(0)
  .max(0xffff_ffff)
  .describe("L2 chain id the offer's proof verifies under; must be the caller's registered L2");
const circuitVersion = z
  .string()
  .min(1)
  .describe('exact circuit version enabled for that L2 chain, e.g. "1.1.0"');
const tributeDraftId = z
  .string()
  .regex(HEX32, "tribute_draft_id must be exactly 32 bytes of 0x-hex")
  .describe("32-byte TributeDraft id bound by the proof and by the caller's L2 attestation");
const suHash = z
  .string()
  .regex(HEX32, "su_hash must be exactly 32 bytes of 0x-hex")
  .describe("32-byte SpendingUnit hash bound by the proof");
const amountMicro = z
  .string()
  .refine((value) => {
    try {
      canonicalAmountMicro(value);
      return true;
    } catch {
      return false;
    }
  }, "amount_micro must be a canonical unsigned u64 below 1000000")
  .describe('six-decimal remainder matching the proof\'s draft (default "0")');

/** Encrypts a Tribute offer to the live offer key, byte-identical to the enclave, and submits it. */
export function registerTributeTools(server: McpServer, ctx: Ctx): void {
  server.tool(
    "tribute_offer",
    "Encrypt and submit a Tribute offer. Reads the DKG-derived offer key from the TeeRegistry, " +
      "auto-detects the OFFERING WorldwideDay if not given, encrypts the payload (X25519 + HKDF-SHA256 + " +
      "ChaCha20Poly1305) and sends offerTribute. Requires OUTBE_PRIVATE_KEY and the ZK offer inputs: a " +
      "combined proof, its Merkle root and the L2 BLS signature over it, the circuit chain/version the " +
      "proof verifies under, plus the tribute_draft_id / su_hashes / amount the proof binds (the enclave " +
      "folds them into the nft_hash checked against the proof). No proof is generated here - produce it " +
      "on the L2. Note: token id is derived from (caller, worldwide_day), so one tribute per account per day.",
    {
      worldwide_day: z.number().int().optional().describe("YYYYMMDD; default = first OFFERING day"),
      amount: tributeBase.optional().describe('whole amount_base matching the proof (default "100")'),
      amount_micro: amountMicro.optional(),
      currency: z.number().int().optional().describe("ISO 4217 numeric, default 840 (USD)"),
      exclude_from_intex_issuance: z
        .boolean()
        .optional()
        .describe("exclude the resulting Tribute from Intex issuance (default false)"),
      zk_proof: zkProof,
      zk_merkle_root: zkMerkleRoot,
      signature: zkSignature,
      l2_chain_id: circuitChainId,
      circuit_version: circuitVersion,
      tribute_draft_id: tributeDraftId,
      su_hashes: z.array(suHash).min(1).describe("SpendingUnit hashes bound by the proof"),
      wait: waitFlag,
    },
    handler(async (a) => {
      if (!ctx.account) throw new Error("set OUTBE_PRIVATE_KEY to submit offers");
      const currency = a.currency ?? 840;
      const excludeFromIntex = a.exclude_from_intex_issuance ?? false;
      const amount = a.amount ?? "100";
      const offerPub = await offerPublicKey(ctx);
      const worldwideDay = a.worldwide_day ?? (await offeringDay(ctx));
      const payload = buildPayload({
        creator: ctx.account.address,
        amount_base: amount,
        amount_micro: a.amount_micro,
        tribute_draft_id: a.tribute_draft_id,
        su_hashes: a.su_hashes,
      });
      const args = offerTributeArgs(encryptOffer(offerPub, payload), {
        worldwideDay,
        currency,
        excludeFromIntex,
        zkProof: a.zk_proof as Hex,
        l2ChainId: a.l2_chain_id,
        circuitVersion: a.circuit_version,
        zkMerkleRoot: a.zk_merkle_root as Hex,
        signature: a.signature as Hex,
      });
      const factory = resolveContract("tributefactory");
      const hash = await sendTx(ctx, { entry: factory, method: "offerTribute", args, gas: GAS_OFFER });
      const meta = {
        txHash: hash,
        offerKey: bytesToHex(offerPub),
        worldwide_day: worldwideDay,
        currency,
        amount_base: amount,
        amount_micro: a.amount_micro ?? "0",
        exclude_from_intex_issuance: excludeFromIntex,
        creator: ctx.account.address,
        l2_chain_id: a.l2_chain_id,
        circuit_version: a.circuit_version,
        tribute_draft_id: a.tribute_draft_id,
        su_hashes: a.su_hashes,
      };
      if (a.wait === false) return ok({ ...meta, status: "submitted" });

      const r = await ctx.publicClient.waitForTransactionReceipt({ hash, timeout: RECEIPT_TIMEOUT_MS });
      const owned =
        r.status === "success" ? await view(ctx, "tribute", "getTributesByOwner", [ctx.account.address]) : null;
      return ok({
        ...meta,
        status: r.status,
        blockNumber: r.blockNumber.toString(),
        gasUsed: r.gasUsed.toString(),
        tributesOwned: owned,
      });
    }),
  );
}
