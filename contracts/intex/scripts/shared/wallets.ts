// Wallet Utilities
// Functions for loading and validating wallet files.

import path from "path";
import { existsSync, readFileSync } from "fs";
import type { WalletEntry } from "../auction/bidders.js";

// =============================================================================
// Constants
// =============================================================================

export const DEFAULT_WALLETS_PATH = path.join(
  process.cwd(),
  "data",
  "bidders",
  "wallets-bsc-testnet.json",
);

export const DEFAULT_COMMITS_PATH = path.join(process.cwd(), "data", "bidders", "commits.json");

// =============================================================================
// Wallet Loading
// =============================================================================

/**
 * Load wallets from JSON file and validate entries.
 * Throws if file not found or entries are invalid.
 */
export function loadWallets(walletsPath: string): WalletEntry[] {
  if (!existsSync(walletsPath)) {
    throw new Error(`Wallets file not found: ${walletsPath}`);
  }

  const wallets = JSON.parse(readFileSync(walletsPath, "utf8")) as WalletEntry[];

  for (const wallet of wallets) {
    if (!wallet.privateKey || !wallet.address) {
      throw new Error(`Invalid wallet entry in ${walletsPath}: missing privateKey or address`);
    }
  }

  console.log(`[wallets] Loaded ${wallets.length} wallets from ${walletsPath}`);
  return wallets;
}
