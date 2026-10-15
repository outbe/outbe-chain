import { chacha20poly1305 } from "@noble/ciphers/chacha";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { type Address, type Hex, bytesToBigInt, concat, formatUnits, getAddress, hexToBytes } from "viem";
import type { Ctx } from "../chain.js";
import type { Network } from "../net/resolver.js";
import { accountKeys } from "./keys.js";
import type { Ledger } from "./mac.js";

/** Confidential balance fields: liquid balance, and Gratis pledged to Credis. */
export const FIELD = { balance: 0, pledged: 1 } as const;

const text = (s: string) => new TextEncoder().encode(s);

/** Opens a Promis `version || ciphertext` blob with the account's view key. An empty blob is zero. */
export function openPromis(view: Uint8Array, account: Address, blob: Hex): bigint {
  const bytes = hexToBytes(blob);
  if (bytes.length === 0) return 0n;
  if (bytes.length !== 56) throw new Error(`Promis balance blob is ${bytes.length} bytes, expected 56`);
  const ikm = concat([hexToBytes(account), new Uint8Array([FIELD.balance]), bytes.slice(0, 8)]);
  const nonce = hkdf(sha256, ikm, view, text("outbe/promis/nonce/v1"), 32).slice(0, 12);
  return bytesToBigInt(chacha20poly1305(view, nonce).decrypt(bytes.slice(8)));
}

/** Opens a Gratis `version || "GRA2" || binding || ciphertext` blob for `field`. An empty blob is zero. */
export function openGratis(view: Uint8Array, account: Address, field: number, blob: Hex): bigint {
  const bytes = hexToBytes(blob);
  if (bytes.length === 0) return 0n;
  const marker = new TextDecoder().decode(bytes.slice(8, 12));
  if (bytes.length !== 92 || marker !== "GRA2") throw new Error("not a Gratis GRA2 balance blob");
  const context = concat([hexToBytes(account), new Uint8Array([field]), bytes.slice(0, 8), bytes.slice(12, 44)]);
  const key = hkdf(sha256, context, view, text("outbe/gratis/amount-key/v2"), 32);
  const nonce = hkdf(sha256, context, key, text("outbe/gratis/amount-nonce/v2"), 32).slice(0, 12);
  return bytesToBigInt(chacha20poly1305(key, nonce).decrypt(bytes.slice(44)));
}

/** A decrypted protocol amount: six decimals. */
export function plain(amount: bigint) {
  return { raw: amount.toString(), value: formatUnits(amount, 6) };
}

/**
 * The signer's own amounts, opened with its view key for `ledger`; undefined for another account
 * or a non-Outbe network. A node that cannot hand out keys yields `{ decryptError }` instead.
 */
export async function signerAmounts(
  ctx: Ctx,
  network: Network,
  ledger: Ledger,
  account: Address,
  open: (view: Uint8Array) => Record<string, bigint>,
): Promise<Record<string, unknown> | undefined> {
  if (!ctx.account || !network.isOutbe || getAddress(account) !== ctx.account.address) return undefined;
  try {
    const { view } = await accountKeys(network, ledger, ctx.account);
    return Object.fromEntries(Object.entries(open(view)).map(([name, amount]) => [name, plain(amount)]));
  } catch (error) {
    return { decryptError: (error as Error).message };
  }
}
