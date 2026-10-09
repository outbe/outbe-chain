import assert from "node:assert/strict";
import nodeCrypto from "node:crypto";
import { syncBuiltinESMExports } from "node:module";
import { readFileSync, writeFileSync } from "node:fs";
import test, { mock } from "node:test";
import {
  type Abi,
  type AbiFunction,
  type Hex,
  encodeAbiParameters,
  pad,
  stringToHex,
  zeroAddress,
} from "viem";
import { ORDER_DATA_TYPE_HASH, encodeOrderData } from "../intent/format.js";
import { DEFAULT_ROUTER, ROUTER_ABI } from "../intent/registry.js";
import {
  AUCTION_ABI,
  DESIS_ABI,
  ERC20_ABI,
  ESCROW_ABI,
  FACTORY_ABI,
  INTEX_ABI,
  NFT_ABI,
  NFT_BRIDGE_ABI,
  ORIGIN_ROUTER_ABI,
  VAULT_ROUTER_ABI,
  intexAddress,
} from "../intex/registry.js";
import { CONTRACTS } from "../registry.js";
import { sample } from "./fake-chain.js";
import { SIGNER, startHarness } from "./harness.js";

const GOLDEN = new URL("./tools.golden.json", import.meta.url);
const OTHER = "0x2222222222222222222222222222222222222222";
const HASH: Hex = `0x${"44".repeat(32)}`;
const SERIES = "20260212-TRY-U";

const image = Buffer.from("<svg/>").toString("base64");
const document = Buffer.from(
  JSON.stringify({ name: "Token", image: `data:image/svg+xml;base64,${image}` }),
).toString("base64");
const DATA_URI = `data:application/json;base64,${document}`;

const ORDER = encodeAbiParameters(
  [{ type: "bytes32" }, { type: "bytes" }],
  [
    ORDER_DATA_TYPE_HASH,
    encodeOrderData({
      sender: pad(SIGNER),
      recipient: pad(SIGNER),
      inputToken: pad(zeroAddress),
      outputToken: pad(zeroAddress),
      amountIn: 5n,
      amountOut: 5n,
      senderNonce: 1n,
      originDomain: 97,
      destinationDomain: 54_322_345,
      destinationSettler: pad(DEFAULT_ROUTER),
      fillDeadline: 1_760_000_100,
      data: "0x",
    }),
  ],
);

const INTEX_ABIS: [Abi, Parameters<typeof intexAddress>[1]][] = [
  [AUCTION_ABI, "auction"],
  [ESCROW_ABI, "escrow"],
  [NFT_ABI, "nft"],
  [NFT_BRIDGE_ABI, "nftBridge"],
  [INTEX_ABI, "intex"],
  [FACTORY_ABI, "factory"],
  [DESIS_ABI, "desis"],
  [VAULT_ROUTER_ABI, "vaultRouter"],
  [ORIGIN_ROUTER_ABI, "originRouter"],
  [ERC20_ABI, "paymentToken"],
];

type Case = [tool: string, args?: Record<string, unknown>];

const json = (value: unknown): unknown =>
  JSON.parse(JSON.stringify(value, (_key, v) => (typeof v === "bigint" ? v.toString() : v)));

/** One `contract_call` per view function of every registry contract: the formatter's whole input space. */
function contractCalls(): Case[] {
  const cases: Case[] = [];
  for (const [name, entry] of Object.entries(CONTRACTS)) {
    for (const item of entry.abi) {
      if (item.type !== "function") continue;
      const fn = item as AbiFunction;
      if (fn.stateMutability !== "view" && fn.stateMutability !== "pure") continue;
      cases.push(["contract_call", { contract: name, method: fn.name, args: json(fn.inputs.map(sample)) }]);
    }
  }
  return cases;
}

const TOOLS: Case[] = [
  ["chain_info"],
  ["block_get"],
  ["block_get", { block: 100 }],
  ["block_get", { block: "safe" }],
  ["transaction_get", { hash: HASH }],
  ["transaction_receipt_get", { hash: HASH }],
  ["tribute_get", { id: "7" }],
  ["tributes_by_owner", { owner: OTHER }],
  ["tributes_by_day", { worldwide_day: 20261009 }],
  ["worldwide_day_totals", { worldwide_day: 20261009 }],
  ["nod_get", { id: "7" }],
  ["nods_by_owner", { owner: OTHER }],
  ["gem_get", { id: "7" }],
  ["gem_position_get", { id: "7" }],
  ["gem_settle_quote", { id: "7", asset: OTHER }],
  ["gems_by_owner", { owner: OTHER }],
  ["credis_position_get", { id: "7" }],
  ["gratis_balance", { account: OTHER }],
  ["promis_balance", { account: OTHER }],
  ["fidelity_max_index", { timestamp: 1_760_000_000 }],
  ["agentreward_claimable", { account: OTHER }],
  ["agentreward_pool_claimable", { account: OTHER, pool: 2 }],
  ["worldwide_days_offering"],
  ["worldwide_day_get", { worldwide_day: 20261009 }],
  ["currency_pairs"],
  ["currency_rate", { base: zeroAddress, quote: OTHER }],
  ["currency_rate_vwap", { base: zeroAddress, quote: OTHER }],
  ["currency_rate_vwap", { base: zeroAddress, quote: OTHER, lookback_seconds: 3600 }],
  ["validators"],
  ["validator_get", { address: OTHER }],
  ["staking_info", { validator: OTHER }],
  ["metacanon_get"],
  ["canon_get"],
  ["oip_get", { id: "1" }],
  ["gip_get", { id: "1" }],
  ["oip_list", { author: OTHER }],
  ["oip_list", { status: "Approved", offset: 1, limit: 2 }],
  ["oip_list", { author: OTHER, status: "Approved" }],
  ["gip_list", { status: "Draft" }],
  ["credis_reserve", { smart_account: OTHER, source: SIGNER, asset: OTHER, amount: "2000000", reference_currency: 978 }],
  ["gratis_pledge", { reservation_id: "1", mac: HASH, op_nonce: "3" }],
  ["gratis_cancel_pledge", { reservation_id: "1" }],
  ["credis_issue", { reservation_id: "1", stake: "1" }],
  ["credis_settle", { position_id: "7", amount: "500000" }],
  [
    "tribute_offer",
    {
      worldwide_day: 20261009,
      zk_proof: "0x0102",
      zk_merkle_root: HASH,
      signature: "0x0304",
      l2_chain_id: 57005,
      circuit_version: "1.1.0",
      tribute_draft_id: HASH,
      su_hashes: [HASH],
    },
  ],
  [
    "tribute_offer",
    {
      zk_proof: "0x0102",
      zk_merkle_root: HASH,
      signature: "0x0304",
      l2_chain_id: 57005,
      circuit_version: "1.1.0",
      tribute_draft_id: HASH,
      su_hashes: [HASH],
      wait: false,
    },
  ],
  ["staking_stake", { validator: OTHER, amount: "1.5" }],
  ["staking_unstake", { amount: "1", wait: false }],
  ["staking_unbonded_claim"],
  ["agentreward_claim", { pool: 1 }],
  ["agentreward_claim", { pool: 0, amount: "2" }],
  ["oracle_feeder_delegate", { feeder: OTHER }],
  ["oracle_vote_submit", { tuples: [{ base: zeroAddress, quote: OTHER, exchangeRate: "2000000", volume: "5" }] }],
  ["intent_order_open", { origin: "outbe-testnet", destination: "bsc-testnet", input_token: "COEN", output_token: "COEN", amount_in: "1" }],
  [
    "intent_order_open",
    {
      origin: "bsc-testnet",
      destination: "outbe-testnet",
      input_token: "USD",
      output_token: "USD",
      amount_in: "2",
      amount_out: "1.5",
      fill_deadline_seconds: 600,
      wait: false,
    },
  ],
  ["intent_order_open", { origin: "mars", destination: "bsc-testnet", input_token: "USD", output_token: "USD", amount_in: "1" }],
  ["intent_order_track", { order_id: HASH, chain: "bsc-testnet" }],
  ["intent_order_refund", { order_id: HASH, chain: "bsc-testnet" }],
  ["intex_series_info", { series: SERIES }],
  ["intex_series_list"],
  ["intex_holdings_by_owner", { account: OTHER }],
  ["intex_series_balance", { series: SERIES, account: OTHER }],
  ["auctions_active", { from_date: 20261005, to_date: 20261009 }],
  ["auctions_active", { include_all: true }],
  ["auction_info", { worldwideDay: 20261009 }],
  ["auction_chains", { worldwideDay: 20261009 }],
  ["auction_bids_by_owner", { worldwideDay: 20261009 }],
  ["auction_bids_by_owner", { account: OTHER }],
  ["auction_bid_commit", { worldwideDay: 20261009, units: 2, rate: "0.8", issuanceCurrency: 949, referenceCurrency: 840 }],
  ["auction_bid_reveal", { worldwideDay: 20261009, units: 2, rate: "0.8", issuanceCurrency: 949, referenceCurrency: 840 }],
  ["auction_bid_reveal", { worldwideDay: 20261009, units: 2, rate: "1.5", issuanceCurrency: 949, referenceCurrency: 840 }],
  ["auction_bid_cancel", { worldwideDay: 20261009 }],
  ["intex_claim_commit_bond", { worldwideDay: 20261009, bidder: OTHER }],
  ["auction_claim_refund", { worldwideDay: 20261009, wait: false }],
  ["intex_payment_allowance", {}],
  ["intex_payment_approve", { amount: "100" }],
  ["intex_payment_approve", { max: true, wait: false }],
  ["intex_payment_approve", {}],
  ["intex_bridge_quote", { series: SERIES, units: "2" }],
  ["intex_bridge_send", { series: SERIES, units: "2", recipient: OTHER }],
  ["intex_settle", { series: SERIES, units: "2", token: OTHER }],
  ["intex_settlement_tokens", { series: SERIES, units: 3 }],
  ["intex_promis_mine", { series: SERIES, units: "2" }],
  ["intex_promis_balance", {}],
  ["intex_series_info", { series: SERIES, network: "solana" }],
];

/** Replaces both randomness sources the offer encryption draws from with a counter. */
function seedRandomness(): void {
  let next = 0;
  const fill = <T extends ArrayBufferView | null>(array: T): T => {
    if (array) new Uint8Array(array.buffer, array.byteOffset, array.byteLength).forEach((_, i, bytes) => (bytes[i] = next++ & 0xff));
    return array;
  };
  mock.method(globalThis.crypto, "getRandomValues", fill);
  mock.method(nodeCrypto, "randomBytes", (size: number) => fill(Buffer.alloc(size)));
  syncBuiltinESMExports();
}

test("every MCP tool keeps its surface and its output against a fixed chain", async () => {
  mock.timers.enable({ apis: ["Date"], now: Date.UTC(2026, 9, 9, 12) });
  seedRandomness();
  const harness = await startHarness((chain) => {
    for (const [name, entry] of Object.entries(CONTRACTS)) chain.register(entry.abi, entry.address);
    for (const network of ["outbe-testnet", "bsc-testnet"]) {
      for (const [abi, key] of INTEX_ABIS) {
        try {
          chain.register(abi, intexAddress(network, key));
        } catch {
          // Not deployed on this network.
        }
      }
    }
    chain.register(ROUTER_ABI, DEFAULT_ROUTER);
    chain.register(ERC20_ABI);
    for (const name of ["tokenURI", "contractURI", "uri"]) chain.reply(name, DATA_URI);
    chain.reply("tributeOfferPublicKey", 9n);
    chain.seed(HASH, 54_322_345, OTHER, "0x1234");
    chain.reply("openOrders", ORDER);
    chain.reply("orderStatus", stringToHex("OPENED", { size: 32 }));
    for (const name of ["totalSeries", "getPairCount"]) chain.reply(name, 3n);
  });
  try {
    const { tools } = await harness.client.listTools();
    const calls = [];
    for (const [tool, args] of [...TOOLS, ...contractCalls()]) {
      const from = harness.chain.sent.length;
      const { text, isError } = await harness.call(tool, args);
      let output: unknown = text;
      try {
        output = JSON.parse(text);
      } catch {
        // Plain-text output stays as text.
      }
      calls.push({ tool, args, isError, output, sent: json(harness.chain.since(from)) });
    }
    const actual = `${JSON.stringify({ tools, calls }, null, 2)}\n`;
    if (process.env.UPDATE_GOLDEN === "1") writeFileSync(GOLDEN, actual);
    assert.equal(actual, readFileSync(GOLDEN, "utf8"));
  } finally {
    await harness.close();
    mock.timers.reset();
    mock.restoreAll();
    syncBuiltinESMExports();
  }
});

