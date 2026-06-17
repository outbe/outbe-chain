// Demo-runbook network config + address resolution (QC-1261 / E0).
//
// Resolves block-explorer links (for evidence), LayerZero endpoint IDs (for delivery proofs), and
// the deployed contract addresses the runbooks drive. Addresses are read from the published
// `@outbe/intex-contracts` package (same source the post-deploy workflow uses), with a local
// `deployed-addresses.json` and per-contract env overrides as fallbacks — so a demo can run against
// a fresh deploy without hand-editing the scripts.

import * as fs from "fs";

export type DemoNetwork = "outbeTestnet" | "outbeTestnetNew" | "outbeDevnet" | "outbePrivnet" | "bscTestnet" | "bsc";

/** LayerZero v2 endpoint IDs. */
export const LZ_EIDS: Record<DemoNetwork, number> = {
  bscTestnet: 40102,
  bsc: 30102,
  outbePrivnet: 40512,
  outbeDevnet: 40712,
  outbeTestnet: 40812,
  outbeTestnetNew: 40912,
};

/** Explorer tx-URL builders. Outbe explorer is env-overridable (`OUTBE_EXPLORER_URL`); when unset, no link is emitted. */
const EXPLORER_TX_BASE: Record<DemoNetwork, string | undefined> = {
  bscTestnet: "https://testnet.bscscan.com/tx/",
  bsc: "https://bscscan.com/tx/",
  outbeTestnet: process.env.OUTBE_EXPLORER_URL,
  outbeTestnetNew: process.env.OUTBE_EXPLORER_URL,
  outbeDevnet: process.env.OUTBE_EXPLORER_URL,
  outbePrivnet: process.env.OUTBE_EXPLORER_URL,
};

/** Build an explorer tx link, or `undefined` if no explorer is configured for the network. */
export function explorerTxUrl(network: DemoNetwork, txHash: string): string | undefined {
  const base = EXPLORER_TX_BASE[network];
  if (!base) return undefined;
  return base.endsWith("/") ? `${base}${txHash}` : `${base}/${txHash}`;
}

export const isOutbe = (network: DemoNetwork): boolean => network.startsWith("outbe");

/** Per-contract env override, e.g. DEMO_ADDR_DESIS=0x... */
const envOverride = (contract: string): string | undefined => process.env[`DEMO_ADDR_${contract.toUpperCase()}`];

function loadPackageAddresses(network: DemoNetwork): Record<string, string> {
  const candidates = [
    `node_modules/@outbe/intex-contracts/dist/addresses/${network}.json`,
    `node_modules/@outbe/intex-contracts/addresses/${network}.json`,
    `deployed-addresses.json`,
  ];
  for (const p of candidates) {
    if (!fs.existsSync(p)) continue;
    try {
      const data = JSON.parse(fs.readFileSync(p, "utf-8"));
      if (data.contracts) return data.contracts as Record<string, string>;
    } catch {
      /* try the next candidate */
    }
  }
  return {};
}

/** Contract addresses a runbook may need, resolved for a network. Missing entries are `undefined`. */
export interface DemoAddresses {
  desis?: string;
  intexFactory?: string;
  originMessenger?: string;
  targetMessenger?: string;
  intexAuction?: string;
  escrowAdapter?: string;
  intexNFT1155?: string;
  promisLimit?: string;
  // External / pre-existing addresses (from DEMO_ADDR_* env overrides):
  paymentToken?: string;
  vaultProvider?: string;
  metadosis?: string;
  theCompact?: string;
}

/** Address keys that resolve from a deployed package / deployed-addresses.json (vs. external file). */
type ContractKey =
  | "desis"
  | "intexFactory"
  | "originMessenger"
  | "targetMessenger"
  | "intexAuction"
  | "escrowAdapter"
  | "intexNFT1155"
  | "promisLimit";

const PACKAGE_KEY: Record<ContractKey, string> = {
  desis: "Desis",
  intexFactory: "IntexFactory",
  originMessenger: "OriginMessenger",
  targetMessenger: "TargetMessenger",
  intexAuction: "IntexAuction",
  escrowAdapter: "EscrowAdapter",
  intexNFT1155: "IntexNFT1155",
  promisLimit: "MockPromisLimit",
};

/**
 * Resolve addresses for `network`. Precedence: env override (`DEMO_ADDR_*`) > deployed package /
 * `deployed-addresses.json` (contracts). External/token addresses come from `DEMO_ADDR_*` only.
 */
export function resolveAddresses(network: DemoNetwork): DemoAddresses {
  const pkg = loadPackageAddresses(network);
  const out: DemoAddresses = {};
  (Object.keys(PACKAGE_KEY) as (keyof typeof PACKAGE_KEY)[]).forEach((k) => {
    out[k] = envOverride(k) ?? pkg[PACKAGE_KEY[k]] ?? (k === "promisLimit" ? pkg.PromisLimit : undefined);
  });
  (["paymentToken", "vaultProvider", "metadosis", "theCompact"] as const).forEach((k) => {
    out[k] = envOverride(k) ?? out[k];
  });
  return out;
}

/** Throw a clear error if a required address is missing. */
export function requireAddress(addrs: DemoAddresses, key: keyof DemoAddresses, network: DemoNetwork): string {
  const v = addrs[key];
  if (!v) {
    throw new Error(
      `Missing address for ${key} on ${network}. Deploy + publish first, or set DEMO_ADDR_${key.toUpperCase()}.`,
    );
  }
  return v;
}
