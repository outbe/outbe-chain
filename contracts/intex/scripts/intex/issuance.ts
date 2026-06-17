// Create IntexNFT1155 series from auction params and mint to recipients.
// Auction must be Completed; caller needs RELAYER_ROLE.

import { readFileSync, existsSync } from "fs";
import { seriesIdToUint32, normalizeSeries } from "../shared/auctionId.js";
import type { Hex } from "viem";

export interface AllocationEntry {
  address: string;
  quantity: string | number | bigint;
}

export interface AllocationFile {
  allocations?: AllocationEntry[];
}

export interface WalletAccount {
  address: `0x${string}`;
}

/** Forced-call trigger parameters — mirrors the on-chain IntexCallTrigger struct */
export interface IntexCallTrigger {
  windowDays: number;
  thresholdDays: number;
  callPriceMinor: bigint;
}

export interface Intex1155IssuanceRuntime {
  auctionRead: {
    getAuctionDetails: (args: [number]) => Promise<readonly [AuctionDataRaw, SubmittedBidRaw[]]>;
  };
  intex1155Write: {
    createSeries: (
      args: [number, number, bigint, bigint, bigint, number, number, IntexCallTrigger],
      opts: { account: WalletAccount },
    ) => Promise<Hex>;
    mintBatch: (
      args: [`0x${string}`[], bigint[], number],
      opts: { account: WalletAccount },
    ) => Promise<Hex>;
  };
  publicClient: { waitForTransactionReceipt: (args: { hash: Hex }) => Promise<void> };
  wallet: { account: WalletAccount };
}

/** Auction data from contract — mirrors the on-chain AuctionData struct */
export interface AuctionDataRaw {
  worldwideDayState: number;
  schedule: {
    commitEnd: number;
    revealEnd: number;
    issuanceEnd: number;
  };
  params: {
    promisLoadMinor: bigint;
    minIntexBidPrice: bigint;
    costAmountMinor: bigint;
    floorPriceMinor: bigint;
    minIntexBidQuantity: number;
  };
  result: {
    issuedIntexLoadedPromis: bigint;
    auctionIntexClearingPrice: bigint;
    issuedIntexCount: number;
    wonBidsCount: number;
  };
}

/** Submitted bid data from contract — mirrors the on-chain SubmittedBidData struct */
export interface SubmittedBidRaw {
  bidderAddress: `0x${string}`;
  intexBidPrice: bigint;
  timestamp: number;
  intexQuantity: number;
}

function loadAllocationsFromFile(path: string): AllocationEntry[] {
  if (!existsSync(path)) {
    throw new Error(`Allocations file not found: ${path}`);
  }
  const raw = JSON.parse(readFileSync(path, "utf8")) as AllocationEntry[] | AllocationFile;
  const list = Array.isArray(raw) ? raw : (raw as AllocationFile).allocations;
  if (!Array.isArray(list) || list.length === 0) {
    throw new Error(`Allocations file must contain an array or { allocations: [] }: ${path}`);
  }
  return list;
}

function toRecipientsAndQuantities(entries: AllocationEntry[]): {
  recipients: `0x${string}`[];
  quantities: bigint[];
} {
  const recipients: `0x${string}`[] = [];
  const quantities: bigint[] = [];
  for (const e of entries) {
    const q = typeof e.quantity === "bigint" ? e.quantity : BigInt(String(e.quantity));
    if (q <= 0n) continue;
    recipients.push(e.address as `0x${string}`);
    quantities.push(q);
  }
  return { recipients, quantities };
}

// ISO 4217 numeric alias of the settlement token (840 = USD).
const DEFAULT_REFERENCE_CURRENCY = 840;
// 0 falls back to the contract default call period (21 days).
const DEFAULT_INTEX_CALL_PERIOD = 0;

export interface Intex1155IssuanceArgs {
  runtime: Intex1155IssuanceRuntime;
  series?: string;
  /** Duration in seconds between Called and the settlement deadline (0 = contract default). */
  intexCallPeriod?: number;
  /** ISO 4217 numeric alias of the settlement token. */
  referenceCurrency?: number;
  /** Forced-call trigger parameters (defaults to a zeroed trigger). */
  callTrigger?: IntexCallTrigger;
  allocationsPath?: string; // JSON path: [{ address, quantity }] or { allocations }. Omit = use revealed bids
  skipCreate?: boolean;
  skipMint?: boolean;
}

export async function runIntex1155IssuanceCore(opts: Intex1155IssuanceArgs): Promise<void> {
  const { runtime, series, allocationsPath, skipCreate, skipMint } = opts;

  if (!series) {
    throw new Error("Provide --series");
  }
  const seriesId = seriesIdToUint32(normalizeSeries(series));

  const [auctionData, bids] = await runtime.auctionRead.getAuctionDetails([seriesId]);

  if (auctionData.result.auctionIntexClearingPrice === 0n) {
    throw new Error(
      "Auction is not completed (auctionIntexClearingPrice is 0). Run executeAuctionClearing first.",
    );
  }

  const promisLoadMinor = auctionData.params.promisLoadMinor;
  const costAmountMinor = auctionData.params.costAmountMinor;
  const floorPriceMinor = auctionData.params.floorPriceMinor;

  const intexCallPeriod = opts.intexCallPeriod ?? DEFAULT_INTEX_CALL_PERIOD;
  const referenceCurrency = opts.referenceCurrency ?? DEFAULT_REFERENCE_CURRENCY;
  const callTrigger: IntexCallTrigger =
    opts.callTrigger ?? { windowDays: 0, thresholdDays: 0, callPriceMinor: 0n };

  console.log("[intex1155-issuance]", {
    seriesId,
    promisLoadMinor: promisLoadMinor.toString(),
    costAmountMinor: costAmountMinor.toString(),
    floorPriceMinor: floorPriceMinor.toString(),
    intexCallPeriod,
    referenceCurrency,
    issuedIntexCount: auctionData.result.issuedIntexCount.toString(),
  });

  if (!skipCreate) {
    const txCreate = await runtime.intex1155Write.createSeries(
      [
        seriesId,
        auctionData.result.issuedIntexCount,
        promisLoadMinor,
        costAmountMinor,
        floorPriceMinor,
        intexCallPeriod,
        referenceCurrency,
        callTrigger,
      ],
      { account: runtime.wallet.account },
    );
    await runtime.publicClient.waitForTransactionReceipt({ hash: txCreate });
    console.log("[intex1155-issuance] createSeries tx:", txCreate);
  } else {
    console.log("[intex1155-issuance] skipCreate: skipping createSeries");
  }

  let recipients: `0x${string}`[];
  let quantities: bigint[];

  if (allocationsPath) {
    const entries = loadAllocationsFromFile(allocationsPath);
    const rq = toRecipientsAndQuantities(entries);
    recipients = rq.recipients;
    quantities = rq.quantities;
  } else {
    const entries: AllocationEntry[] = (bids as SubmittedBidRaw[]).map((b) => ({
      address: b.bidderAddress,
      quantity: b.intexQuantity,
    }));
    const rq = toRecipientsAndQuantities(entries);
    recipients = rq.recipients;
    quantities = rq.quantities;
    if (recipients.length === 0) {
      console.log("[intex1155-issuance] No revealed bids and no --allocations; skipping mint.");
      return;
    }
    console.log("[intex1155-issuance] Using allocations from revealed bids:", recipients.length, "recipients");
  }

  if (!skipMint && recipients.length > 0) {
    const txMint = await runtime.intex1155Write.mintBatch(
      [recipients, quantities, seriesId],
      { account: runtime.wallet.account },
    );
    await runtime.publicClient.waitForTransactionReceipt({ hash: txMint });
    console.log("[intex1155-issuance] mintBatch tx:", txMint, "recipients:", recipients.length);
  } else if (skipMint) {
    console.log("[intex1155-issuance] skipMint: skipping mintBatch");
  }

  console.log("[intex1155-issuance] done.");
}
