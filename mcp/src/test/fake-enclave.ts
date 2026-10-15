import { chacha20poly1305 } from "@noble/ciphers/chacha";
import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { type Address, type Hex, bytesToHex, concat, hexToBytes, numberToBytes, recoverMessageAddress } from "viem";
import { deriveKeysMessage } from "../confidential/keys.js";
import type { Ledger } from "../confidential/mac.js";
import type { FakeChain } from "./fake-chain.js";

export const VIEW_KEY = new Uint8Array(32).fill(0x5a);
export const MODIFY_KEY = new Uint8Array(32).fill(0x6b);

/** Answers `outbe_deriveKeys` like the enclave: checks the account signed, then seals fixed keys to the requester. */
export function fakeEnclave(chain: FakeChain, calls: Ledger[] = []): void {
  const enclaveSecret = new Uint8Array(32).fill(0x01);
  chain.rpc("outbe_deriveKeys", async (params) => {
    const [ledger, account, ephemeralPubkey, signature] = params as [Ledger, Address, Hex, Hex];
    const clientPub = hexToBytes(ephemeralPubkey);
    const message = deriveKeysMessage(ledger, account, clientPub);
    if ((await recoverMessageAddress({ message: { raw: message }, signature })) !== account) {
      throw new Error("signature does not control account");
    }
    calls.push(ledger);
    const shared = x25519.getSharedSecret(enclaveSecret, clientPub);
    const key = hkdf(sha256, shared, clientPub, new TextEncoder().encode("outbe/tee/dkg-share/v1"), 32);
    const nonce = new Uint8Array(12).fill(9);
    return {
      sealed: bytesToHex(chacha20poly1305(key, nonce).encrypt(concat([VIEW_KEY, MODIFY_KEY]))),
      nonce: bytesToHex(nonce),
      enclaveEphemeralPubkey: bytesToHex(x25519.getPublicKey(enclaveSecret)),
    };
  });
}

const text = (s: string) => new TextEncoder().encode(s);

/** A Promis balance blob for `account` under `VIEW_KEY`, as the enclave writes it. */
export function promisBlob(account: Address, amount: bigint): Hex {
  const version = numberToBytes(1n, { size: 8 });
  const ikm = concat([hexToBytes(account), new Uint8Array([0]), version]);
  const nonce = hkdf(sha256, ikm, VIEW_KEY, text("outbe/promis/nonce/v1"), 32).slice(0, 12);
  return bytesToHex(concat([version, chacha20poly1305(VIEW_KEY, nonce).encrypt(numberToBytes(amount, { size: 32 }))]));
}

/** A Gratis GRA2 blob for `account`'s `field` under `VIEW_KEY`, as the enclave writes it. */
export function gratisBlob(account: Address, field: number, amount: bigint): Hex {
  const version = numberToBytes(1n, { size: 8 });
  const binding = new Uint8Array(32).fill(0x0b);
  const context = concat([hexToBytes(account), new Uint8Array([field]), version, binding]);
  const key = hkdf(sha256, context, VIEW_KEY, text("outbe/gratis/amount-key/v2"), 32);
  const nonce = hkdf(sha256, context, key, text("outbe/gratis/amount-nonce/v2"), 32).slice(0, 12);
  const sealed = chacha20poly1305(key, nonce).encrypt(numberToBytes(amount, { size: 32 }));
  return bytesToHex(concat([version, text("GRA2"), binding, sealed]));
}
