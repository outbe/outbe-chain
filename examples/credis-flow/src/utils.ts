import { config } from "dotenv";
import { dirname, resolve } from "path";
import { ethers } from "ethers";
import { fileURLToPath } from "url";

export const DEFAULT_ENV = "local-reth";

export const DEFAULT_GRATIS_ADDRESS = "0x0000000000000000000000000000000000001003";
export const DEFAULT_GRATIS_FACTORY_ADDRESS = "0x0000000000000000000000000000000000002003";
export const DEFAULT_CREDIS_FACTORY_ADDRESS = "0x0000000000000000000000000000000000001009";
export const DEFAULT_CREDIS_ADDRESS = "0x000000000000000000000000000000000000100A";

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
