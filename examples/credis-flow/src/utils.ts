import { config } from "dotenv";
import { dirname, resolve } from "path";
import { ethers } from "ethers";
import { fileURLToPath } from "url";

export const DEFAULT_ENV = "local-reth";

export const DEFAULT_GRATIS_ADDRESS = "0x0000000000000000000000000000000000001003";
export const DEFAULT_GRATIS_FACTORY_ADDRESS = "0x0000000000000000000000000000000000002003";
export const DEFAULT_GRATIS_POOL_ADDRESS = "0x0000000000000000000000000000000000002004";
export const DEFAULT_CREDIS_FACTORY_ADDRESS = "0x0000000000000000000000000000000000001009";
export const DEFAULT_CREDIS_ADDRESS = "0x000000000000000000000000000000000000100A";

// Gratis denomination ladder. Ids are assigned in ascending amount order and
// must match `crates/core/gratispool/src/constants.rs::DenomAmount`.
//
// `id = 0` is intentionally invalid on-chain. `id = 1` (Gratis0_1, 0.1 GRATIS)
// is a RESERVED anadosis-only sub-rung: `pledgeGratis` rejects it. It exists
// only as the destination for a single anadosis installment's reclaim note —
// one decade below the pledge denom, worth `pledge_amount / 10`.
export const GRATIS_DENOMINATIONS: { id: number; amount: bigint; pledgeable: boolean }[] = [
  { id: 1, amount: 1n * 10n ** 17n, pledgeable: false }, // Gratis0_1 — reserved (anadosis only)
  { id: 2, amount: 1n * 10n ** 18n, pledgeable: true }, // Gratis1
  { id: 3, amount: 10n * 10n ** 18n, pledgeable: true }, // Gratis10
  { id: 4, amount: 100n * 10n ** 18n, pledgeable: true }, // Gratis100
  { id: 5, amount: 1_000n * 10n ** 18n, pledgeable: true }, // Gratis1k
  { id: 6, amount: 10_000n * 10n ** 18n, pledgeable: true }, // Gratis10k
];

// Resolve the anadosis (one-decade-down) denomination for a per-installment
// reclaim note by its amount. The amount equals `pledge_amount / 10`, which is
// exactly the `gratisAmount` reported by `getNextAnadosis`. The reclaim
// commitment MUST be computed with the returned `id`, or the note is inserted
// but permanently unspendable (the chain stores it opaquely).
export function anadosisDenomByAmount(amount: bigint): { id: number; amount: bigint } {
  const d = GRATIS_DENOMINATIONS.find((x) => x.amount === amount);
  if (!d) {
    throw new Error(
      `No denomination in the ladder matches anadosis amount ${amount}; cannot build the reclaim note.`,
    );
  }
  return d;
}

export interface TokenMeta {
  decimals: number;
  symbol: string;
}

export function formatToken(value: bigint, decimals: number, symbol: string): string {
  return `${ethers.formatUnits(value, decimals)} ${symbol}`;
}

export function formatTokenMeta(value: bigint, meta: TokenMeta): string {
  return formatToken(value, meta.decimals, meta.symbol);
}

export function formatTokenMeta2(value: bigint, meta: TokenMeta): string {
  return `${ethers.formatUnits(value, meta.decimals)}`;
}

export function formatTokenDiff(value: bigint, decimals: number, symbol: string): string {
  return `${value >= 0n ? "+" : ""}${formatToken(value, decimals, symbol)}`;
}

export async function fetchTokenMeta(
  contract: { decimals(): Promise<bigint>; symbol(): Promise<string> },
): Promise<TokenMeta> {
  const [decimalsBig, symbol] = await Promise.all([
    contract.decimals(),
    contract.symbol(),
  ]);
  return { decimals: Number(decimalsBig), symbol };
}

export function loadEnv(importMetaUrl: string, envName: string, opts?: { deploymentEnv?: boolean }): {
  envPath: string;
  deploymentEnvPath?: string;
} {
  const callerFilename = fileURLToPath(importMetaUrl);
  const callerDirname = dirname(callerFilename);
  const envPath = resolve(callerDirname, `../.${envName}.env`);
  config({ path: envPath, override: true });

  let deploymentEnvPath: string | undefined;
  if (opts?.deploymentEnv) {
    deploymentEnvPath = resolve(callerDirname, `../.${envName}.deployment.env`);
    config({ path: deploymentEnvPath, override: true });
    config({ path: envPath, override: true });
  }

  return { envPath, deploymentEnvPath };
}

export function requireEnv(name: string, context?: string): string {
  const val = process.env[name];
  if (!val) throw new Error(`${name} is not set${context ? ` in ${context}` : ""}`);
  return val;
}
