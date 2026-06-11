// Off-chain crypto for the shielded gratis pool.
//
// Mirrors `crates/core/gratispool/src/state.rs` and the
// `outbe-commitment-nullifier-circuit` Noir program shipped by
// `outbe-circuits` field-element-by-field-element. Poseidon parity
// (circomlibjs <-> outbe-poseidon <-> noir-lang/poseidon) is the
// load-bearing assumption: any divergence permanently un-spends the
// pledged gratis. The pledge script asserts parity against the on-chain
// `CommitmentInserted` event before reporting success.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import { dirname, resolve } from "path";
import { fileURLToPath } from "url";
import { buildPoseidon, type Poseidon } from "circomlibjs";
import { Noir, type CompiledCircuit } from "@noir-lang/noir_js";
import { Barretenberg, UltraHonkBackend } from "@aztec/bb.js";

// ---------------------------------------------------------------------------
// Pinned outbe-circuits version
// ---------------------------------------------------------------------------
//
// Single source of truth for which `outbe-circuits` release the
// circuit ACIR and canonical VK are pulled from. Bump this in lockstep
// with the Rust workspace pin in `/Cargo.toml` (the three
// `outbe-zk-*` / `outbe-crypto-common` entries pinned by `tag =`).
//
// The bb.js + noir_js versions in `package.json` must also align with
// the Noir + Barretenberg toolchain `outbe-circuits` was compiled
// against (`outbe-circuits/Cargo.toml`: `acvm`/`nargo` tag and
// `barretenberg-rs` nightly pin). Cross-version drift surfaces as a
// VK / proof byte mismatch rather than a compile error, so keep these
// in sync manually when bumping.
export const OUTBE_CIRCUITS_VERSION = "v0.10.0";
export const OUTBE_CIRCUITS_REPO = "outbe/outbe-circuits";

const OUTBE_CIRCUITS_RAW_BASE =
  `https://raw.githubusercontent.com/${OUTBE_CIRCUITS_REPO}/${OUTBE_CIRCUITS_VERSION}`;

// Tags & action constants — must match crates/core/gratispool/src/constants.rs.
export const TAG_COMMIT_GRATIS = 0x6e0001n;
export const TAG_NULLIFIER_GRATIS = 0x6e0002n;
export const TAG_MERKLE_GRATIS = 0x6e0003n;
export const TAG_BINDING = 0x6e0004n;
export const ACTION_REQUEST_CREDIS = 1n;
export const ACTION_UNPLEDGE = 2n;
export const MERKLE_DEPTH = 20;

// BN254 scalar field modulus. Matches `Fr` in `state.rs`.
export const BN254_FR_MODULUS =
  21888242871839275222246405745257275088548364400416034343698204186575808495617n;

// ---------------------------------------------------------------------------
// Field arithmetic
// ---------------------------------------------------------------------------

export function mod(x: bigint, p: bigint = BN254_FR_MODULUS): bigint {
  const r = x % p;
  return r >= 0n ? r : r + p;
}

export function toField(input: bigint | Uint8Array | string): bigint {
  if (typeof input === "bigint") return mod(input);
  if (typeof input === "string") return mod(BigInt(input));
  if (input.length !== 32) {
    throw new Error(`toField: expected 32 bytes, got ${input.length}`);
  }
  let x = 0n;
  for (const b of input) x = (x << 8n) | BigInt(b);
  return mod(x);
}

export function addressToField(addr: string): bigint {
  const norm = addr.toLowerCase().replace(/^0x/, "");
  if (norm.length !== 40) throw new Error(`addressToField: bad address ${addr}`);
  return mod(BigInt("0x" + norm));
}

export function fieldToHex32(x: bigint): string {
  return "0x" + mod(x).toString(16).padStart(64, "0");
}

// ---------------------------------------------------------------------------
// Poseidon — circomlibjs, Circom-compatible BN254.
// ---------------------------------------------------------------------------

let poseidonPromise: Promise<Poseidon> | null = null;

async function getPoseidon(): Promise<Poseidon> {
  if (!poseidonPromise) poseidonPromise = buildPoseidon();
  return poseidonPromise;
}

export async function poseidonHash(inputs: bigint[]): Promise<bigint> {
  const h = await getPoseidon();
  const out = h(inputs.map((x) => mod(x)));
  return BigInt(h.F.toString(out));
}

// ---------------------------------------------------------------------------
// Domain-tagged hashes
// ---------------------------------------------------------------------------

export async function commitmentHash(
  secret: bigint,
  nullifierSecret: bigint,
  denomId: number,
): Promise<bigint> {
  return poseidonHash([
    TAG_COMMIT_GRATIS,
    secret,
    nullifierSecret,
    BigInt(denomId),
  ]);
}

export async function nullifierHash(nullifierSecret: bigint): Promise<bigint> {
  return poseidonHash([TAG_NULLIFIER_GRATIS, nullifierSecret]);
}

export async function receiverBinding(
  actionTag: bigint,
  target: string,
  chainId: bigint,
  nonce: bigint,
): Promise<bigint> {
  return poseidonHash([
    TAG_BINDING,
    actionTag,
    addressToField(target),
    chainId,
    nonce,
  ]);
}

// Inner-node hash. Tag folds into the *left* input via field addition.
export async function merkleNode(left: bigint, right: bigint): Promise<bigint> {
  return poseidonHash([mod(TAG_MERKLE_GRATIS + left), right]);
}

// ---------------------------------------------------------------------------
// Incremental Merkle tree (Tornado-style, depth 20)
// ---------------------------------------------------------------------------

let merkleZerosCache: bigint[] | null = null;

// NOTE: empty-subtree hashes use a *different* Poseidon than `merkleNode` —
// arity-3 with TAG as a separate input. Mirrors `state.rs::merkle_zeros`:
//     parent = poseidon-3(TAG_MERKLE_GRATIS, prev, prev)
// Internal nodes (`merkleNode`) use arity-2 with TAG folded into the left
// input. The two are NOT interchangeable: zeros[1] != merkleNode(0, 0).
export async function merkleZeros(): Promise<bigint[]> {
  if (merkleZerosCache) return merkleZerosCache;
  const zeros: bigint[] = [0n];
  for (let i = 0; i < MERKLE_DEPTH; i++) {
    zeros.push(await poseidonHash([TAG_MERKLE_GRATIS, zeros[i], zeros[i]]));
  }
  merkleZerosCache = zeros;
  return zeros;
}

export interface MerkleProof {
  siblings: bigint[]; // length MERKLE_DEPTH
  root: bigint;
  index: number;
}

// Build a fresh Merkle proof by replaying every commitment ever appended to
// `denom_id`'s tree and pulling the path from `leafIndex` to the root. Matches
// the on-chain append walk in `state.rs::GratisPoolContract::append_leaf`.
export async function buildMerkleProof(
  commitments: bigint[],
  leafIndex: number,
): Promise<MerkleProof> {
  if (leafIndex < 0 || leafIndex >= commitments.length) {
    throw new Error(
      `buildMerkleProof: leafIndex ${leafIndex} out of range (have ${commitments.length} commitments)`,
    );
  }
  const zeros = await merkleZeros();

  // level 0 = leaves (padded with zero leaves up to one past leafIndex; we only
  // need siblings on the path, so right padding via zeros[level] is enough).
  let current: bigint[] = commitments.slice();

  const siblings: bigint[] = [];
  let idx = leafIndex;
  for (let level = 0; level < MERKLE_DEPTH; level++) {
    const isRight = (idx & 1) === 1;
    const siblingIdx = isRight ? idx - 1 : idx + 1;
    const sibling =
      siblingIdx < current.length ? current[siblingIdx] : zeros[level];
    siblings.push(sibling);

    // Compute the next level by hashing pairs; pad the right with zeros[level].
    const next: bigint[] = [];
    for (let i = 0; i < current.length; i += 2) {
      const left = current[i];
      const right = i + 1 < current.length ? current[i + 1] : zeros[level];
      next.push(await merkleNode(left, right));
    }
    current = next;
    idx >>= 1;
  }

  return { siblings, root: current[0], index: leafIndex };
}

// ---------------------------------------------------------------------------
// Canonical artefact loader (circuit ACIR + VK)
// ---------------------------------------------------------------------------

// Both the compiled Noir program (used to build the prover witness) and
// the canonical UltraHonkKeccak VK (used by `_writevk.ts` to cross-
// check what the on-chain `verify_ultra_honk_keccak` sees) live in the
// `outbe-circuits` repo at deterministic paths. We resolve them via:
//
//   1. Local download cache under `.outbe-circuits-cache/<version>/` —
//      the steady-state path once an artefact has been fetched once.
//   2. Sibling `outbe-circuits` checkout — zero-config fallback for
//      circuit devs iterating on `.nr` sources locally; no download
//      needed if you have a checkout at the expected sibling path.
//   3. GitHub raw download from
//      `raw.githubusercontent.com/<repo>/<version>/<path>` — written
//      back into the cache so subsequent runs hit (1).
//
// The cache dir is `.gitignored`.

const CACHE_DIR = resolve(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  ".outbe-circuits-cache",
  OUTBE_CIRCUITS_VERSION,
);

const CIRCUIT_JSON_REPO_PATH =
  "crates/outbe-zk-circuit-noir/data/commitment_nullifier_proof.json";
const VK_REPO_PATH = "crates/outbe-zk-canonical/res/vks/commitment_nullifier.vk";

const CIRCUIT_JSON_CACHE_NAME = "commitment_nullifier_proof.json";
const VK_CACHE_NAME = "commitment_nullifier.vk";

async function fetchCanonicalAsset(
  repoRelativePath: string,
  cacheFilename: string,
): Promise<string> {
  const cachePath = resolve(CACHE_DIR, cacheFilename);
  if (existsSync(cachePath)) return cachePath;

  const url = `${OUTBE_CIRCUITS_RAW_BASE}/${repoRelativePath}`;
  console.error(
    `[outbe-circuits ${OUTBE_CIRCUITS_VERSION}] fetching ${cacheFilename}`,
  );
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(
      `failed to download ${url}: ${response.status} ${response.statusText}`,
    );
  }
  const buf = Buffer.from(await response.arrayBuffer());
  mkdirSync(CACHE_DIR, { recursive: true });
  writeFileSync(cachePath, buf);
  return cachePath;
}

let circuitCache: CompiledCircuit | null = null;

export async function loadCircuit(): Promise<CompiledCircuit> {
  if (circuitCache) return circuitCache;
  const path = await fetchCanonicalAsset(
    CIRCUIT_JSON_REPO_PATH,
    CIRCUIT_JSON_CACHE_NAME,
  );
  const raw = readFileSync(path, "utf-8");
  circuitCache = JSON.parse(raw) as CompiledCircuit;
  return circuitCache;
}

// Canonical UltraHonkKeccak VK shipped by `outbe-zk-canonical` for the
// commitment-nullifier circuit at this pinned version. `_writevk.ts`
// regenerates a fresh VK from local bytecode and writes it to the same
// cache slot so the two can be compared byte-for-byte.
export async function loadCommitmentNullifierVk(): Promise<Uint8Array> {
  const path = await fetchCanonicalAsset(VK_REPO_PATH, VK_CACHE_NAME);
  return new Uint8Array(readFileSync(path));
}

export function commitmentNullifierVkCachePath(): string {
  return resolve(CACHE_DIR, VK_CACHE_NAME);
}

export interface ProveUnpledgeInputs {
  secret: bigint;
  nullifierSecret: bigint;
  denomId: number;
  merklePath: bigint[]; // length MERKLE_DEPTH, sibling at each level
  merkleIndex: number;
  merkleRoot: bigint;
  nullifierHashValue: bigint;
  receiverBindingValue: bigint;
}

// Run Noir to build the witness, then Barretenberg to generate an
// UltraHonkKeccak proof. Returns the bare proof bytes — the on-chain verifier
// in `verifier.rs` prepends the public-input prefix itself, so we hand back
// `proofData.proof` unchanged.
//
// Public-input order matches the v0.10.0 `outbe-commitment-nullifier-circuit`
// `fn main`:
//   merkle_root, nullifier_hash, denom_id, receiver_binding,
//   tag_commit, tag_nullifier, tag_merkle.
// The three `tag_*` values are public inputs (not in-circuit constants) so the
// verifier pins them to the deployment-fixed `TAG_*_GRATIS` triple per call.
export async function proveUnpledge(
  inputs: ProveUnpledgeInputs,
): Promise<Uint8Array> {
  const circuit = await loadCircuit();
  const noir = new Noir(circuit);
  const witnessInput = {
    merkle_root: fieldToHex32(inputs.merkleRoot),
    nullifier_hash: fieldToHex32(inputs.nullifierHashValue),
    denom_id: fieldToHex32(BigInt(inputs.denomId)),
    receiver_binding: fieldToHex32(inputs.receiverBindingValue),
    tag_commit: fieldToHex32(TAG_COMMIT_GRATIS),
    tag_nullifier: fieldToHex32(TAG_NULLIFIER_GRATIS),
    tag_merkle: fieldToHex32(TAG_MERKLE_GRATIS),
    secret: fieldToHex32(inputs.secret),
    nullifier_secret: fieldToHex32(inputs.nullifierSecret),
    merkle_path: inputs.merklePath.map(fieldToHex32),
    merkle_index: fieldToHex32(BigInt(inputs.merkleIndex)),
  };
  const { witness } = await noir.execute(witnessInput);

  const api = await Barretenberg.new({ threads: 1 });
  try {
    const backend = new UltraHonkBackend(circuit.bytecode, api);
    // verifierTarget 'evm' = keccak transcript with ZK. The Rust verifier
    // `verify_ultra_honk_keccak(_, vk, /* disable_zk */ false)` in
    // `crates/core/gratispool/src/verifier.rs` keeps ZK enabled (the third
    // arg is `disable_zk: bool`, despite the misleading comment that calls
    // it `is_recursive`), matching `bb write_vk -t evm` which produces a
    // ZK-compatible VK.
    const proofData = await backend.generateProof(witness, {
      verifierTarget: "evm",
    });
    return proofData.proof;
  } finally {
    await api.destroy();
  }
}
