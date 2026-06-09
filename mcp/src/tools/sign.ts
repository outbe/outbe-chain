import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { type Hex, bytesToHex, parseUnits, toBytes } from "viem";
import { z } from "zod";
import { type Ctx, sendTx } from "../chain.js";
import { buildPayload, encryptOffer } from "../crypto.js";
import { CONTRACTS, resolveContract } from "../registry.js";
import { handler, ok, view } from "./util.js";

const addr = z.string().describe("0x-prefixed address");
const coen = z.string().describe("amount in whole COEN, e.g. \"100\" or \"1.5\"");

const GAS_OFFER = 8_000_000n;
const GAS_DEFAULT = 3_000_000n;
const GAS_VOTE = 5_000_000n;

/** Send a curated write and optionally wait for the receipt. */
async function submit(
  ctx: Ctx,
  contract: string,
  method: string,
  args: unknown[],
  gas: bigint,
  wait: boolean,
) {
  const entry = resolveContract(contract);
  const hash = await sendTx(ctx, entry, method, args, gas);
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
      "ChaCha20Poly1305) and sends offerTribute. Requires OUTBE_PRIVATE_KEY. Note: token id is derived " +
      "from (caller, worldwide_day), so one tribute per account per day.",
    {
      worldwide_day: z.number().int().optional().describe("YYYYMMDD; default = first OFFERING day"),
      amount: coen.optional(),
      currency: z.number().int().optional().describe("ISO 4217 numeric, default 840 (USD)"),
      wait: z.boolean().optional().describe("wait for the receipt (default true)"),
    },
    handler(async ({ worldwide_day, amount, currency, wait }) => {
      if (!ctx.account) throw new Error("set OUTBE_PRIVATE_KEY to submit offers");
      const cur = currency ?? 840;
      const amt = amount ?? "100";

      const tee = resolveContract("teeregistry");
      const bootstrapped = await ctx.publicClient.readContract({
        address: tee.address,
        abi: tee.abi,
        functionName: "isBootstrapped",
      });
      if (!bootstrapped) throw new Error("TeeRegistry not bootstrapped — no offer key yet");

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
        worldwide_day: day,
        currency: cur,
        amount_base: amt,
      });
      const enc = encryptOffer(offerPub, payload);

      const args = [
        bytesToHex(enc.cipherText),
        bytesToHex(enc.nonce),
        enc.ephemeralPubkey,
        cur,
        "0x" as Hex,
        "0x" as Hex,
        "0x" as Hex,
        "0x" as Hex,
      ];

      const factory = resolveContract("tributefactory");
      const hash = await sendTx(ctx, factory, "offerTribute", args, GAS_OFFER);
      const meta = {
        txHash: hash,
        offerKey: bytesToHex(offerPub),
        worldwide_day: day,
        currency: cur,
        amount_base: amt,
        creator: ctx.account.address,
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
    }),
  );

  // --- staking ---------------------------------------------------------------
  server.tool(
    "staking_stake",
    "Stake COEN to a validator. Requires OUTBE_PRIVATE_KEY.",
    { validator: addr, amount: coen, wait: z.boolean().optional() },
    handler(({ validator, amount, wait }) =>
      submit(ctx, "staking", "stake", [validator, parseUnits(amount, 18)], GAS_DEFAULT, wait ?? true),
    ),
  );

  server.tool(
    "staking_unstake",
    "Unstake COEN (starts unbonding). Requires OUTBE_PRIVATE_KEY.",
    { amount: coen, wait: z.boolean().optional() },
    handler(({ amount, wait }) =>
      submit(ctx, "staking", "unstake", [parseUnits(amount, 18)], GAS_DEFAULT, wait ?? true),
    ),
  );

  server.tool(
    "staking_claim_unbonded",
    "Claim unbonded stake after the unbonding period. Requires OUTBE_PRIVATE_KEY.",
    { wait: z.boolean().optional() },
    handler(({ wait }) =>
      submit(ctx, "staking", "claimUnbonded", [], GAS_DEFAULT, wait ?? true),
    ),
  );

  // --- rewards / agentreward -------------------------------------------------
  server.tool(
    "rewards_claim",
    "Claim pending validator rewards. Requires OUTBE_PRIVATE_KEY.",
    { wait: z.boolean().optional() },
    handler(({ wait }) => submit(ctx, "rewards", "claimRewards", [], GAS_DEFAULT, wait ?? true)),
  );

  server.tool(
    "agentreward_claim",
    "Claim AgentReward balance. amount in COEN. Requires OUTBE_PRIVATE_KEY.",
    { amount: coen, wait: z.boolean().optional() },
    handler(({ amount, wait }) =>
      submit(ctx, "agentreward", "claimReward", [parseUnits(amount, 18)], GAS_DEFAULT, wait ?? true),
    ),
  );

  // --- oracle ----------------------------------------------------------------
  server.tool(
    "oracle_delegate_feeder",
    "Delegate oracle feeder consent to an address. Requires OUTBE_PRIVATE_KEY (validator).",
    { feeder: addr, wait: z.boolean().optional() },
    handler(({ feeder, wait }) =>
      submit(ctx, "oracle", "delegateFeederConsent", [feeder], GAS_DEFAULT, wait ?? true),
    ),
  );

  server.tool(
    "oracle_submit_vote",
    "Submit oracle exchange-rate votes. `tuples`: [{base, quote, exchangeRate, volume}] with rate/volume as " +
      "integer minor strings (1e18 scale). Requires OUTBE_PRIVATE_KEY (validator).",
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
