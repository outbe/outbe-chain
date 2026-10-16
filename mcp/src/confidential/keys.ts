import { chacha20poly1305 } from "@noble/ciphers/chacha";
import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { type Address, type Hex, bytesToHex, concat, getAddress, hexToBytes } from "viem";
import type { LocalAccount } from "viem/accounts";
import type { Network } from "../net/resolver.js";
import type { Ledger } from "./mac.js";

/** A ledger's per-account keys: `view` decrypts balances, `modify` authorizes writes. */
export interface AccountKeys {
  view: Uint8Array;
  modify: Uint8Array;
}

/** Keys sealed to the client's ephemeral X25519 key, as `outbe_deriveKeys` returns them. */
export interface SealedKeys {
  sealed: Hex;
  nonce: Hex;
  enclaveEphemeralPubkey: Hex;
}

const SHARE_INFO = new TextEncoder().encode("outbe/tee/dkg-share/v1");

export function deriveKeysMessage(ledger: Ledger, account: Address, ephemeralPubkey: Uint8Array): Uint8Array {
  const tag = new TextEncoder().encode(`outbe/${ledger.toLowerCase()}/derive-keys/v1`);
  return concat([tag, hexToBytes(getAddress(account)), ephemeralPubkey]);
}

/** Opens the sealed keys: the HKDF salt is the client's own ephemeral public key. */
export function unsealKeys(ephemeralSecret: Uint8Array, response: SealedKeys): AccountKeys {
  const ephemeralPubkey = x25519.getPublicKey(ephemeralSecret);
  const shared = x25519.getSharedSecret(ephemeralSecret, hexToBytes(response.enclaveEphemeralPubkey));
  const key = hkdf(sha256, shared, ephemeralPubkey, SHARE_INFO, 32);
  const plaintext = chacha20poly1305(key, hexToBytes(response.nonce)).decrypt(hexToBytes(response.sealed));
  if (plaintext.length !== 64) throw new Error(`sealed keys are ${plaintext.length} bytes, expected 64`);
  return { view: plaintext.slice(0, 32), modify: plaintext.slice(32) };
}

const cache = new Map<string, Promise<AccountKeys>>();

function cacheKey(network: Network, ledger: Ledger, account: Address): string {
  return `${network.chainId}:${ledger}:${getAddress(account)}`;
}

async function requestKeys(network: Network, ledger: Ledger, account: LocalAccount): Promise<AccountKeys> {
  const ephemeralSecret = x25519.utils.randomPrivateKey();
  const ephemeralPubkey = x25519.getPublicKey(ephemeralSecret);
  const signature = await account.signMessage({
    message: { raw: deriveKeysMessage(ledger, account.address, ephemeralPubkey) },
  });
  const response = (await network.client.request({
    method: "outbe_deriveKeys",
    params: [ledger, account.address, bytesToHex(ephemeralPubkey), signature],
  } as never)) as SealedKeys;
  return unsealKeys(ephemeralSecret, response);
}

/** The signer's keys for `ledger` on `network`, derived once per process. Keys differ per chain. */
export function accountKeys(network: Network, ledger: Ledger, account: LocalAccount): Promise<AccountKeys> {
  if (!network.isOutbe) throw new Error(`${ledger} keys exist only on outbe, not on ${network.name}`);
  const key = cacheKey(network, ledger, account.address);
  let keys = cache.get(key);
  if (!keys) {
    keys = requestKeys(network, ledger, account);
    keys.catch(() => cache.delete(key));
    cache.set(key, keys);
  }
  return keys;
}

/** Drops cached keys after the chain rejects them, e.g. once the network re-ran its key setup. */
export function forgetKeys(network: Network, ledger: Ledger, account: Address): void {
  cache.delete(cacheKey(network, ledger, account));
}
