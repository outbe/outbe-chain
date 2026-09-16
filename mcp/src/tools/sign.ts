import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Hex, bytesToHex, toBytes } from "viem";
import { z } from "zod";
import { type Ctx, parseNativeAmount, sendTx } from "../chain.js";
import { buildPayload, canonicalAmountBase, canonicalAmountMicro, encryptOffer } from "../crypto.js";
import { CONTRACTS, resolveContract } from "../registry.js";
import { handler, ok, view } from "./util.js";

const addr = z.string().describe("0x-prefixed address");
const coen = z.string().describe("amount in whole COEN, e.g. \"100\" or \"1.5\"");
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

const GAS_OFFER = 8_000_000n;
const GAS_DEFAULT = 3_000_000n;
const GAS_VOTE = 5_000_000n;

/** `0x`-hex of any width; validity of the bytes themselves is the precompile's call. */
const HEX = /^0x([0-9a-fA-F]{2})*$/;
/** `0x`-hex of exactly 32 bytes - the enclave parses these fields as fixed-width. */
const HEX32 = /^0x(?:[0-9a-fA-F]{2}){32}$/;

const zkProof = z
  .string()
  .regex(HEX, "zk_proof must be 0x-prefixed hex")
  .describe(
    "combined FullProof bytes (0x-hex): 4-byte public-input word count, public inputs, then proof",
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

/** Send a curated write and optionally wait for the receipt. */
async function submit(
  ctx: Ctx,
  contract: string,
  method: string,
  args: unknown[],
  gas: bigint,
  wait: boolean,
  value = 0n,
) {
  const entry = resolveContract(contract);
  const hash = await sendTx(ctx, entry, method, args, gas, value);
  if (!wait) return ok({ txHash: hash, contract, method, status: "submitted" });
  const r = await ctx.publicClient.waitForTransactionReceipt({ hash, timeout: 180_000 });
  return ok({
    txHash: hash,
    contract,
    method,
    status: r.status,
    blockNumber: r.blockNumber.toString(),
    gasUsed: r.gasUsed.toString(),
  });
}

export function registerSignTools(server: McpServer, ctx: Ctx): void {
  // --- tribute_offer (encrypts to the live offer key, byte-identical to enclave)
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
      wait: z.boolean().optional().describe("wait for the receipt (default true)"),
    },
    handler(
      async ({
        worldwide_day,
        amount,
        amount_micro,
        currency,
        exclude_from_intex_issuance,
        zk_proof,
        zk_merkle_root,
        signature,
        l2_chain_id,
        circuit_version,
        tribute_draft_id,
        su_hashes,
        wait,
      }) => {
        if (!ctx.account) throw new Error("set OUTBE_PRIVATE_KEY to submit offers");
        const cur = currency ?? 840;
        const excludeFromIntex = exclude_from_intex_issuance ?? false;
        const amt = amount ?? "100";

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
        const offerPub = toBytes(offerKeyU256, { size: 32 });

        let day = worldwide_day;
        if (day === undefined) {
          const md = resolveContract("metadosis");
          const offering = (await ctx.publicClient.readContract({
            address: md.address,
            abi: md.abi,
            functionName: "getWorldwideDaysByStatus",
            args: [2],
          })) as readonly number[];
          if (!offering.length) throw new Error("no WorldwideDay is currently in OFFERING status");
          day = Number(offering[0]);
        }

        const payload = buildPayload({
          creator: ctx.account.address,
          amount_base: amt,
          amount_micro,
          tribute_draft_id,
          su_hashes,
        });
        const enc = encryptOffer(offerPub, payload);

        const args = [
          bytesToHex(enc.cipherText),
          bytesToHex(enc.nonce),
          enc.ephemeralPubkey,
          day, // worldwideDay
          cur, // tributeCurrency
          cur, // referenceCurrency - a separate axis, same value here
          excludeFromIntex,
          zk_proof as Hex,
          l2_chain_id, // chainId the proof verifies under
          circuit_version, // exact enabled circuit version for that chain
          "0x" as Hex, // zkPublicKey - the combined proof carries its own
          zk_merkle_root as Hex,
          signature as Hex,
        ];

        const factory = resolveContract("tributefactory");
        const hash = await sendTx(ctx, factory, "offerTribute", args, GAS_OFFER);
        const meta = {
          txHash: hash,
          offerKey: bytesToHex(offerPub),
          worldwide_day: day,
          currency: cur,
          amount_base: amt,
          amount_micro: amount_micro ?? "0",
          exclude_from_intex_issuance: excludeFromIntex,
          creator: ctx.account.address,
          l2_chain_id,
          circuit_version,
          tribute_draft_id,
          su_hashes,
        };
        if (wait === false) return ok({ ...meta, status: "submitted" });

        const r = await ctx.publicClient.waitForTransactionReceipt({ hash, timeout: 180_000 });
        const owned =
          r.status === "success" ? await view(ctx, "tribute", "getTributesByOwner", [ctx.account.address]) : null;
        return ok({
          ...meta,
          status: r.status,
          blockNumber: r.blockNumber.toString(),
          gasUsed: r.gasUsed.toString(),
          tributesOwned: owned,
        });
      },
    ),
  );

  // --- staking ---------------------------------------------------------------
  server.tool(
    "staking_stake",
    "Stake COEN to a validator. Requires OUTBE_PRIVATE_KEY.",
    { validator: addr, amount: coen, wait: z.boolean().optional() },
    handler(({ validator, amount, wait }) => {
      const stake = parseNativeAmount(ctx.chain, amount);
      return submit(
        ctx,
        "staking",
        "stake",
        [validator, stake],
        GAS_DEFAULT,
        wait ?? true,
        stake,
      );
    }),
  );

  server.tool(
    "staking_unstake",
    "Unstake COEN (starts unbonding). Requires OUTBE_PRIVATE_KEY.",
    { amount: coen, wait: z.boolean().optional() },
    handler(({ amount, wait }) =>
      submit(
        ctx,
        "staking",
        "unstake",
        [parseNativeAmount(ctx.chain, amount)],
        GAS_DEFAULT,
        wait ?? true,
      ),
    ),
  );

  server.tool(
    "staking_unbonded_claim",
    "Claim unbonded stake after the unbonding period. Requires OUTBE_PRIVATE_KEY.",
    { wait: z.boolean().optional() },
    handler(({ wait }) =>
      submit(ctx, "staking", "claimUnbonded", [], GAS_DEFAULT, wait ?? true),
    ),
  );

  // --- agentreward -----------------------------------------------------------
  // The Rewards precompile (EE03) exposes no callable methods - validator
  // emission is paid in gems (crates/system/rewards/src/precompile.rs).
  server.tool(
    "agentreward_claim",
    "Claim AgentReward from one pool (0 = WAA, 1 = SRA) as a Gem. Omit amount to claim the whole pool balance. Requires OUTBE_PRIVATE_KEY.",
    {
      pool: z.number().int().min(0).max(1).describe("0 = WAA, 1 = SRA"),
      amount: coen.optional().describe("amount to claim; omit for the whole balance"),
      wait: z.boolean().optional(),
    },
    handler(({ pool, amount, wait }) =>
      submit(
        ctx,
        "agentreward",
        "claimReward",
        [pool, amount === undefined ? 0n : parseNativeAmount(ctx.chain, amount)],
        GAS_DEFAULT,
        wait ?? true,
      ),
    ),
  );

  // --- oracle ----------------------------------------------------------------
  server.tool(
    "oracle_feeder_delegate",
    "Delegate oracle feeder consent to an address. Requires OUTBE_PRIVATE_KEY (validator).",
    { feeder: addr, wait: z.boolean().optional() },
    handler(({ feeder, wait }) =>
      submit(ctx, "oracle", "delegateFeederConsent", [feeder], GAS_DEFAULT, wait ?? true),
    ),
  );

  server.tool(
    "oracle_vote_submit",
    "Submit oracle exchange-rate votes. `tuples`: [{base, quote, exchangeRate, volume}] with rate/volume as " +
      "integer minor strings (COEN/ISO rates use scale 1e6; generic pairs keep their existing scale). " +
      "Requires OUTBE_PRIVATE_KEY (validator).",
    {
      tuples: z
        .array(
          z.object({
            base: z.string(),
            quote: z.string(),
            exchangeRate: z.string(),
            volume: z.string(),
          }),
        )
        .min(1),
      wait: z.boolean().optional(),
    },
    handler(({ tuples, wait }) => {
      const t = tuples.map((x) => ({
        base: x.base,
        quote: x.quote,
        exchangeRate: BigInt(x.exchangeRate),
        volume: BigInt(x.volume),
      }));
      return submit(ctx, "oracle", "submitVote", [t], GAS_VOTE, wait ?? true);
    }),
  );

  void CONTRACTS;
}
