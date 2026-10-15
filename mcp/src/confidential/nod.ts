import { chacha20poly1305 } from "@noble/ciphers/chacha";
import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { type Address, type Hex, bytesToBigInt, concat, getAddress, hexToBytes, keccak256, numberToBytes } from "viem";
import type { Ctx } from "../chain.js";
import type { Network } from "../net/resolver.js";
import { signer } from "./authorize.js";
import { CONTRACTS } from "../registry.js";
import { creatorKey } from "../tribute/creator-key.js";

/** The `INod.getNod` fields the owner needs to open the Gratis amount. */
export interface EncryptedNod {
  nodId: bigint;
  owner: Address;
  worldwideDay: number;
  leagueId: number;
  entryPriceMinor: bigint;
  issuanceCurrency: number;
  referenceCurrency: number;
  chainId: bigint;
  encryptionBinding: Hex;
  encryptedCreatorPublicKey: Hex;
  encryptedGratisAmount: Hex;
}

const text = (s: string) => new TextEncoder().encode(s);
const BLOB_LEN = 8 + 32 + 16;

function contextDigest(nod: EncryptedNod): Uint8Array {
  const terms = keccak256(
    concat([
      text("outbe/nod/terms/v2"),
      numberToBytes(nod.chainId, { size: 8 }),
      numberToBytes(nod.nodId, { size: 32 }),
      hexToBytes(nod.owner),
      numberToBytes(nod.worldwideDay, { size: 4 }),
      numberToBytes(nod.leagueId, { size: 2 }),
      numberToBytes(nod.entryPriceMinor, { size: 32 }),
      numberToBytes(nod.issuanceCurrency, { size: 2 }),
      numberToBytes(nod.referenceCurrency, { size: 2 }),
    ]),
    "bytes",
  );
  return keccak256(concat([text("outbe/nod/context/v2"), terms, hexToBytes(nod.encryptionBinding)]), "bytes");
}

/** The ChaCha20-Poly1305 cipher a Nod's Gratis amount is sealed with, as the creator derives it. */
export function nodAmountCipher(creatorSecret: Uint8Array, networkPublicKey: Uint8Array, nod: EncryptedNod) {
  const digest = contextDigest(nod);
  const shared = x25519.getSharedSecret(creatorSecret, networkPublicKey);
  const creatorBlob = hexToBytes(nod.encryptedCreatorPublicKey);
  const info = concat([text("outbe/nod/amount-key/v2"), digest, networkPublicKey, x25519.getPublicKey(creatorSecret), creatorBlob]);
  const key = hkdf(sha256, shared, digest, info, 32);
  const nonceContext = concat([digest.slice(0, 20), new Uint8Array([0]), numberToBytes(1n, { size: 8 })]);
  const nonce = hkdf(sha256, nonceContext, key, text("outbe/nod/amount-nonce/v2"), 32).slice(0, 12);
  return chacha20poly1305(key, nonce);
}

/** Opens a Nod's Gratis amount with the creator key its Tribute was offered under. */
export function openNodAmount(creatorSecret: Uint8Array, networkPublicKey: Uint8Array, nod: EncryptedNod): bigint {
  for (const blob of [nod.encryptedCreatorPublicKey, nod.encryptedGratisAmount].map((b) => hexToBytes(b))) {
    if (blob.length !== BLOB_LEN || bytesToBigInt(blob.slice(0, 8)) !== 1n) throw new Error("invalid encrypted Nod encoding");
  }
  const sealed = hexToBytes(nod.encryptedGratisAmount).slice(8);
  return bytesToBigInt(nodAmountCipher(creatorSecret, networkPublicKey, nod).decrypt(sealed));
}

/** The signer opens its own Nod with the creator key and the network's current Tribute offer key. */
export async function signerNodAmount(ctx: Ctx, network: Network, nod: EncryptedNod): Promise<bigint> {
  const account = signer(ctx);
  if (getAddress(nod.owner) !== account.address) throw new Error(`nod ${nod.nodId} belongs to ${nod.owner}, not the signer`);
  if (nod.chainId !== BigInt(network.chainId)) throw new Error(`nod ${nod.nodId} was issued on chain ${nod.chainId}`);
  const networkKey = await network.client.readContract({
    address: CONTRACTS.teeregistry.address,
    abi: CONTRACTS.teeregistry.abi,
    functionName: "tributeOfferPublicKey",
  });
  const { secret } = await creatorKey(account, network.chainId);
  return openNodAmount(secret, numberToBytes(networkKey as bigint, { size: 32 }), nod);
}
