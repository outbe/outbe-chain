// Client-side crypto for the confidential (TEE-encrypted) Gratis token.
//
// Byte-for-byte mirror of the enclave engine
// (`bin/outbe-tee-enclave/src/{gratis.rs,crypto.rs}` + the HKDF/label constants
// in `crates/system/tee/src/lib.rs`). Any divergence means a balance won't
// decrypt or a write authorization is rejected, so keep these in lockstep.
//
// Crypto primitives use Node's built-in `crypto` (HKDF-SHA256, HMAC-SHA256,
// ChaCha20-Poly1305) plus `@noble/curves` for raw-byte X25519 ECDH.

import { createHmac, hkdfSync, createDecipheriv } from "node:crypto";
import { x25519 } from "@noble/curves/ed25519";
import { ethers } from "ethers";

// GratisOp discriminants — MUST match `outbe_tee::protocol::GratisOp` order.
export enum GratisOp {
  Mine = 0,
  Burn = 1,
  Pledge = 2,
  Unpledge = 3,
  PledgeToBundle = 4,
  UnlockToEoa = 5,
}

// Domain-separation labels — MUST match the Rust constants.
const DKG_SHARE_INFO = utf8("outbe/tee/dkg-share/v1"); // crypto.rs
const NONCE_INFO = utf8("outbe/gratis/nonce/v1"); // lib.rs GRATIS_NONCE_INFO
const MODIFY_TAG = utf8("outbe/gratis/modify/v1"); // gratis.rs MODIFY_PREIMAGE_TAG
const SPEND_BIND_TAG = utf8("outbe/gratis/credis-bind/v1"); // gratis.rs SPEND_BIND_TAG

const FIELD_BALANCE = 0;
const FIELD_PLEDGED = 1;

// ---------------------------------------------------------------------------
// Byte helpers
// ---------------------------------------------------------------------------

function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}

/** Big-endian encode `n` into exactly `bytes` bytes. */
function be(n: bigint, bytes: number): Uint8Array {
  const out = new Uint8Array(bytes);
  let x = n;
  for (let i = bytes - 1; i >= 0; i--) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  if (x !== 0n) throw new Error(`be: ${n} does not fit in ${bytes} bytes`);
  return out;
}

function bytesToBigIntBE(b: Uint8Array): bigint {
  let x = 0n;
  for (const byte of b) x = (x << 8n) | BigInt(byte);
  return x;
}

/** The 20 raw bytes of an address. */
function addressBytes(addr: string): Uint8Array {
  return ethers.getBytes(ethers.getAddress(addr));
}

// ---------------------------------------------------------------------------
// Primitives (Node built-ins)
// ---------------------------------------------------------------------------

/** HKDF-SHA256 extract+expand to 32 bytes — matches `crypto.rs::hkdf_sha256`. */
function hkdf32(salt: Uint8Array, ikm: Uint8Array, info: Uint8Array): Uint8Array {
  return new Uint8Array(hkdfSync("sha256", ikm, salt, info, 32));
}

function hmacSha256(key: Uint8Array, msg: Uint8Array): Uint8Array {
  return new Uint8Array(createHmac("sha256", Buffer.from(key)).update(Buffer.from(msg)).digest());
}

/**
 * ChaCha20-Poly1305 decrypt with empty AAD. Ring appends the 16-byte tag to the
 * ciphertext (`seal_in_place_append_tag`), so we split it off for Node's API.
 */
function chachaDecrypt(key: Uint8Array, nonce: Uint8Array, ctWithTag: Uint8Array): Uint8Array {
  const tag = ctWithTag.slice(ctWithTag.length - 16);
  const ct = ctWithTag.slice(0, ctWithTag.length - 16);
  const d = createDecipheriv("chacha20-poly1305", key, nonce, { authTagLength: 16 });
  d.setAuthTag(tag);
  return new Uint8Array(Buffer.concat([d.update(ct), d.final()]));
}

// ---------------------------------------------------------------------------
// Key delivery — outbe_deriveGratisKeys RPC
// ---------------------------------------------------------------------------

export interface GratisKeys {
  viewKey: Uint8Array; // decrypts this account's balance/pledged ciphertext
  modifyKey: Uint8Array; // authorizes writes (never decrypts)
}

/**
 * Fetch `account`'s enclave-derived view + modify keys via the
 * `outbe_deriveGratisKeys` RPC. The enclave seals them to a fresh client
 * ephemeral X25519 key (the `decrypt_share` scheme in `crypto.rs`).
 *
 * SECURITY NOTE: the current RPC does not authenticate that the requester
 * controls `account` — see the RPC TODO. For the demo we only ever request the
 * user's own keys.
 */
export async function deriveGratisKeys(
  provider: ethers.JsonRpcProvider,
  account: string,
): Promise<GratisKeys> {
  const ephSecret = x25519.utils.randomPrivateKey();
  const ephPublic = x25519.getPublicKey(ephSecret);

  const resp: { sealed: string; nonce: string; enclaveEphemeralPubkey: string } =
    await provider.send("outbe_deriveGratisKeys", [
      ethers.getAddress(account),
      ethers.hexlify(ephPublic),
    ]);

  const sealed = ethers.getBytes(resp.sealed);
  const nonce = ethers.getBytes(resp.nonce);
  const enclaveEph = ethers.getBytes(resp.enclaveEphemeralPubkey);

  // decrypt_share: shared = ephSecret · enclaveEph; key = HKDF(salt=ephPublic, ikm=shared).
  const shared = x25519.getSharedSecret(ephSecret, enclaveEph);
  const key = hkdf32(ephPublic, shared, DKG_SHARE_INFO);
  const plaintext = chachaDecrypt(key, nonce, sealed);
  if (plaintext.length !== 64) {
    throw new Error(`deriveGratisKeys: expected 64 bytes (view||modify), got ${plaintext.length}`);
  }
  return { viewKey: plaintext.slice(0, 32), modifyKey: plaintext.slice(32, 64) };
}

// ---------------------------------------------------------------------------
// Balance / pledged decryption (view key, client-side)
// ---------------------------------------------------------------------------

function decryptField(viewKey: Uint8Array, account: string, field: number, blobHex: string): bigint {
  const blob = ethers.getBytes(blobHex);
  if (blob.length === 0) return 0n; // fresh slot
  const version = blob.slice(0, 8); // version(8 BE)
  const ct = blob.slice(8);
  // slot nonce = HKDF(salt=view_key, ikm=account||field||version)[..12]
  const ikm = concat(addressBytes(account), Uint8Array.of(field), version);
  const nonce = hkdf32(viewKey, ikm, NONCE_INFO).slice(0, 12);
  const pt = chachaDecrypt(viewKey, nonce, ct);
  return bytesToBigIntBE(pt);
}

/** Decrypt an account's `balanceOf(...)` ciphertext blob into a bigint. */
export function decryptBalance(viewKey: Uint8Array, account: string, blobHex: string): bigint {
  return decryptField(viewKey, account, FIELD_BALANCE, blobHex);
}

/** Decrypt an account's `pledgedOf(...)` ciphertext blob into a bigint. */
export function decryptPledged(viewKey: Uint8Array, account: string, blobHex: string): bigint {
  return decryptField(viewKey, account, FIELD_PLEDGED, blobHex);
}

// ---------------------------------------------------------------------------
// Write authorization (modify key, client-side)
// ---------------------------------------------------------------------------

/**
 * The modify-key MAC a write must carry:
 * `HMAC(modify_key, "modify/v1" || account || op || amount || opNonce || chainId)`.
 * `opNonce` MUST equal the account's current on-chain `op_nonce`.
 */
export function modifyMac(
  modifyKey: Uint8Array,
  account: string,
  op: GratisOp,
  amount: bigint,
  opNonce: bigint,
  chainId: bigint,
): string {
  const preimage = concat(
    MODIFY_TAG,
    addressBytes(account),
    Uint8Array.of(op),
    be(amount, 32),
    be(opNonce, 8),
    be(chainId, 32),
  );
  return ethers.hexlify(hmacSha256(modifyKey, preimage));
}

/**
 * The per-pledge spend secret the EOA derives from its modify key + the public
 * pledge handle, then hands to the CCA off-chain: `HMAC(modify_key, handle)`.
 */
export function pledgeSecret(modifyKey: Uint8Array, handleHex: string): Uint8Array {
  const handle = ethers.getBytes(handleHex);
  if (handle.length !== 32) throw new Error("pledgeSecret: handle must be 32 bytes");
  return hmacSha256(modifyKey, handle);
}

/**
 * The spend authorization binding a pledge to a destination bundle account:
 * `HMAC(pledge_secret, "credis-bind" || bundle)`. Prevents a mempool observer of
 * `requestCredis(handle, spendAuth)` from redirecting the loan.
 */
export function spendAuth(secret: Uint8Array, bundle: string): string {
  return ethers.hexlify(hmacSha256(secret, concat(SPEND_BIND_TAG, addressBytes(bundle))));
}

// ---------------------------------------------------------------------------
// Position id — keccak256(handle || bundleAccount), matches CredisContract
// ---------------------------------------------------------------------------

/** `position_id = keccak256(pledge_handle(32) || bundle_account(20))` as uint256. */
export function positionId(handleHex: string, bundleAccount: string): bigint {
  const handle = ethers.getBytes(handleHex);
  if (handle.length !== 32) throw new Error("positionId: handle must be 32 bytes");
  return BigInt(ethers.keccak256(concat(handle, addressBytes(bundleAccount))));
}
