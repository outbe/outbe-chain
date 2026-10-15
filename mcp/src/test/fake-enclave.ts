import { chacha20poly1305 } from "@noble/ciphers/chacha";
import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { type Address, type Hex, bytesToHex, concat, hexToBytes, recoverMessageAddress } from "viem";
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
