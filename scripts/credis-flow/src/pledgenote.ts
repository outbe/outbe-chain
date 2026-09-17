// Reuse the Rust codec/crypto. Private inputs never enter command-line arguments.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ethers } from "ethers";
import type { GratisKeys } from "./confidential.js";

export interface PrivateReceipt {
  note_id: string;
  secret: string;
  terms: unknown;
  balance: string;
  pledged: string;
  next_nonce: number;
  rcfi: string;
  efficiency: string;
  league: number;
}

function localCodec<T>(operation: "prepare" | "decrypt", input: unknown): T {
  const directory = mkdtempSync(join(tmpdir(), "outbe-pledgenote-"));
  try {
    const source = join(directory, "input.json");
    const output = join(directory, "output.json");
    writeFileSync(source, JSON.stringify(input), { mode: 0o600, flag: "wx" });
    execFileSync(process.env.OUTBE_CLI || "outbe-cli", ["pledge-note", operation, "--input", source, "--output", output], { stdio: ["ignore", "ignore", "pipe"] });
    return JSON.parse(readFileSync(output, "utf8")) as T;
  } finally {
    rmSync(directory, { recursive: true });
  }
}

export async function queryGratis(provider: ethers.JsonRpcProvider, keys: GratisKeys, account: string): Promise<PrivateReceipt> {
  const tee = new ethers.Contract("0x000000000000000000000000000000000000EE0A", ["function tributeOfferPublicKey() view returns (uint256)"], provider);
  const chain = (await provider.getNetwork()).chainId;
  if (chain > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("chain ID exceeds JSON integer precision");
  const prepared = localCodec<{to: string; data: string}>("prepare", {
    operation: "owner", chain_id: Number(chain), offer_public: ethers.toBeHex(await tee.tributeOfferPublicKey(), 32),
    account, modify_key: ethers.hexlify(keys.modifyKey), nonce: 0, action: "Query",
  });
  const returned = await provider.call(prepared);
  const [encrypted_receipt] = ethers.AbiCoder.defaultAbiCoder().decode(["bytes"], returned);
  return localCodec<PrivateReceipt>("decrypt", {view_key: ethers.hexlify(keys.viewKey), encrypted_receipt});
}
