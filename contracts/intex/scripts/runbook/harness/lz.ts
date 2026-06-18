// LayerZero delivery-await + proof for the demo runbooks (QC-1261 / E0).
//
// A cross-chain step is only "done" once the destination chain has actually processed the message.
// We prove that the same way the protocol orders messages: poll the destination endpoint's
// `lazyInboundNonce` until it reaches the source endpoint's `outboundNonce` for the (sender, dstEid,
// receiver) channel. Ported from scripts/auction/crosschainFlow.ts `waitForLzDelivery`, made
// self-contained (clients + addresses passed in) for reuse across the auction/settlement runbooks.

import { type Address, type PublicClient } from "viem";
import { type DemoNetwork, LZ_EIDS } from "./config.js";
import { type LzProof } from "./report.js";

/** Custom LZ V2 EndpointV2 — same CREATE2 address on every network (BSC, Outbe). */
const LZ_ENDPOINT = "0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2" as Address;

const ENDPOINT_NONCE_ABI = [
  {
    type: "function",
    name: "outboundNonce",
    stateMutability: "view",
    inputs: [
      { name: "_sender", type: "address" },
      { name: "_dstEid", type: "uint32" },
      { name: "_receiver", type: "bytes32" },
    ],
    outputs: [{ type: "uint64" }],
  },
  {
    type: "function",
    name: "lazyInboundNonce",
    stateMutability: "view",
    inputs: [
      { name: "_receiver", type: "address" },
      { name: "_srcEid", type: "uint32" },
      { name: "_sender", type: "bytes32" },
    ],
    outputs: [{ type: "uint64" }],
  },
] as const;

const addressToBytes32 = (a: Address): `0x${string}` =>
  `0x${"0".repeat(24)}${a.slice(2).toLowerCase()}` as `0x${string}`;

export interface AwaitLzArgs {
  srcNetwork: DemoNetwork;
  dstNetwork: DemoNetwork;
  /** Public client connected to the source chain. */
  srcPublic: PublicClient;
  /** Public client connected to the destination chain. */
  dstPublic: PublicClient;
  /** OApp on the source chain (the sender, e.g. OriginMessenger). */
  srcOApp: Address;
  /** OApp on the destination chain (the receiver / peer, e.g. TargetMessenger). */
  dstOApp: Address;
  pollIntervalMs?: number;
  maxPolls?: number;
}

/**
 * Block until the source `outboundNonce` is delivered on the destination
 * (`lazyInboundNonce >= outboundNonce`) and return the proof. Throws on timeout.
 */
export async function awaitLzDelivery(args: AwaitLzArgs): Promise<LzProof> {
  const srcEid = LZ_EIDS[args.srcNetwork];
  const dstEid = LZ_EIDS[args.dstNetwork];
  const receiverBytes32 = addressToBytes32(args.dstOApp);
  const senderBytes32 = addressToBytes32(args.srcOApp);

  const outbound = (await args.srcPublic.readContract({
    address: LZ_ENDPOINT,
    abi: ENDPOINT_NONCE_ABI,
    functionName: "outboundNonce",
    args: [args.srcOApp, dstEid, receiverBytes32],
  })) as bigint;

  const pollMs = args.pollIntervalMs ?? 5_000;
  const maxPolls = args.maxPolls ?? 120;
  console.log(`[lz] awaiting outbound nonce ${outbound} (eid ${srcEid} -> ${dstEid})`);

  for (let i = 0; i < maxPolls; i++) {
    const delivered = (await args.dstPublic.readContract({
      address: LZ_ENDPOINT,
      abi: ENDPOINT_NONCE_ABI,
      functionName: "lazyInboundNonce",
      args: [args.dstOApp, srcEid, senderBytes32],
    })) as bigint;

    if (delivered >= outbound) {
      console.log(`[lz] delivered (lazyInboundNonce=${delivered} >= outbound=${outbound})`);
      return { srcEid, dstEid, outboundNonce: outbound.toString(), deliveredNonce: delivered.toString() };
    }
    if (i % 6 === 0) console.log(`[lz]   delivered=${delivered}, waiting for ${outbound}...`);
    await new Promise((r) => setTimeout(r, pollMs));
  }
  throw new Error(
    `LZ delivery timeout: outbound nonce ${outbound} not delivered (eid ${srcEid} -> ${dstEid}) after ${maxPolls} polls`,
  );
}

const INBOUND_DROPPED_EVENT = {
  type: "event",
  name: "InboundMessageDropped",
  inputs: [
    { name: "guid", type: "bytes32", indexed: true },
    { name: "srcEid", type: "uint32", indexed: true },
    { name: "reason", type: "bytes", indexed: false },
  ],
} as const;

export interface InboundDropped {
  guid: `0x${string}`;
  srcEid: number;
  reason: `0x${string}`;
  blockNumber: bigint;
  transactionHash: `0x${string}`;
}

/**
 * Scan recent blocks of the destination messenger for `InboundMessageDropped`
 * (emitted by the `drop-don't-block` try/catch in `_lzReceive`). Use when a
 * downstream state check fails after `awaitLzDelivery` succeeds — the LZ layer
 * delivered the packet but the inbound dispatch reverted and was caught.
 */
export async function findInboundDropped(
  dstPublic: PublicClient,
  messenger: Address,
  lookbackBlocks: bigint = 200n,
): Promise<InboundDropped[]> {
  const head = await dstPublic.getBlockNumber();
  const fromBlock = head > lookbackBlocks ? head - lookbackBlocks : 0n;
  const logs = await dstPublic.getLogs({
    address: messenger,
    event: INBOUND_DROPPED_EVENT,
    fromBlock,
    toBlock: head,
  });
  return logs.map((l) => {
    const a = (l as { args: { guid: `0x${string}`; srcEid: number; reason: `0x${string}` } }).args;
    return {
      guid: a.guid,
      srcEid: Number(a.srcEid),
      reason: a.reason,
      blockNumber: l.blockNumber as bigint,
      transactionHash: l.transactionHash as `0x${string}`,
    };
  });
}

/** Best-effort decode of an `InboundMessageDropped` reason: try ASCII first, fall back to hex. */
export function formatDroppedReason(reason: `0x${string}`): string {
  if (reason === "0x" || reason.length < 10) return reason;
  // 4-byte error selector + ABI-encoded args. Show selector for caller to lookup.
  const selector = reason.slice(0, 10);
  return `selector=${selector} payload=${reason}`;
}

export interface CrossChainAssertArgs {
  /** Label printed alongside expected/actual (e.g. `BNB IntexAuction stage`). */
  label: string;
  /** Reads the destination-side state and returns it as a string. */
  read: () => Promise<string>;
  /** Expected value (string match against `read()`). */
  expected: string;
  /** Destination messenger address; scanned for `InboundMessageDropped` on mismatch. */
  dstMessenger: Address;
  /** Destination public client (same instance used for `awaitLzDelivery`). */
  dstPublic: PublicClient;
  pollIntervalMs?: number;
  maxPolls?: number;
}

/**
 * Verify destination-side state by polling after `awaitLzDelivery`. The LZ nonce advances on
 * DVN verification, but executor execution of `_lzReceive` can land in a later block — so the
 * destination state may not be visible the instant the nonce check passes. Poll the read until
 * the expected value lands or the timeout is hit; on timeout, scan the destination messenger for
 * `InboundMessageDropped` events and surface the dropped guid/reason.
 */
export async function assertCrossChainState(args: CrossChainAssertArgs): Promise<{ ok: boolean; label: string; expected: string; actual: string }> {
  const pollMs = args.pollIntervalMs ?? 5_000;
  const maxPolls = args.maxPolls ?? 36;
  let lastActual = "<not yet read>";
  console.log(`[lz] ${args.label}: polling for "${args.expected}" (every ${pollMs}ms, up to ${maxPolls} times)`);
  for (let i = 0; i < maxPolls; i++) {
    try {
      lastActual = await args.read();
      if (lastActual === args.expected) {
        console.log(`[lz] ${args.label}: matched on poll ${i + 1}`);
        return { ok: true, label: args.label, expected: args.expected, actual: lastActual };
      }
    } catch (err) {
      // Read reverted — common while waiting for executor (e.g. `getAuctionInfo` → `AuctionNotFound`
      // until `auctionStart` runs). Keep polling; only a final timeout is fatal.
      lastActual = `<revert: ${(err as Error).message?.split("\n")[0] ?? "unknown"}>`;
    }
    if (i % 6 === 0) console.log(`[lz] ${args.label}: poll ${i + 1}/${maxPolls}, actual=${lastActual}`);
    await new Promise((r) => setTimeout(r, pollMs));
  }
  const dropped = await findInboundDropped(args.dstPublic, args.dstMessenger);
  let extra = ` (no InboundMessageDropped on ${args.dstMessenger} in recent blocks)`;
  if (dropped.length > 0) {
    const last = dropped[dropped.length - 1];
    extra = ` — InboundMessageDropped guid=${last.guid} reason=${formatDroppedReason(last.reason)} (tx ${last.transactionHash})`;
  }
  throw new Error(`${args.label} mismatch after ${maxPolls} polls: expected ${args.expected}, last actual ${lastActual}.${extra}`);
}
