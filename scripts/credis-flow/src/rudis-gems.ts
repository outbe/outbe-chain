#!/usr/bin/env -S npx tsx
// Rehearsal wallet operations. PayNote proving stays in the canonical Rust CLI.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, openSync, closeSync, readFileSync, readdirSync,
  renameSync, unlinkSync, writeFileSync, fsyncSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { ethers } from "ethers";
import { decryptBalance, deriveKeys, findPowNonce, modifyMac, PromisOp } from "./confidential.js";

export const CHAIN_ID = 70860602n;
export const WUSDC = "0xdD9eD2f161c4F9A642471BCF49331A95F5B2B1d3";
const GEM = "0x0000000000000000000000000000000000001013";
const FACTORY = "0x0000000000000000000000000000000000002013";
const PROMIS = "0x0000000000000000000000000000000000001337";
const PROMIS_FACTORY = "0x0000000000000000000000000000000000002337";
const PAYNOTE = "0x0000000000000000000000000000000000001019";
const ROOT = fileURLToPath(new URL("../../../", import.meta.url));
const STATES = ["Issued", "Qualified", "Called", "Settled"];
const NATIVE_PER_PROMIS_MINOR = 1_000_000_000_000n;

export const GEM_ABI = [
  "function balanceOf(address) view returns (uint256)",
  "function tokenOfOwnerByIndex(address,uint256) view returns (uint256)",
  "function getGemStatus(uint256) view returns (tuple(uint256 gemId,address owner,uint8 gemType,uint8 state,uint256 promisLoad,uint256 entryPrice,uint256 floorPrice,uint16 issuanceCurrency,uint16 referenceCurrency,uint64 issuedAt,uint256 callPrice,uint64 calledAt,uint32 callNoticePeriod))",
];
export const FACTORY_ABI = [
  "function quoteSettlement(uint256,address) view returns (uint16,uint256)",
  "function settleGem(uint256,bytes)",
  "function minePromis(uint256,uint64,bytes32,uint64) returns (uint256)",
  "event GemSettled(uint256 indexed gemId,address owner,uint256 amountPaid,uint16 settlementCurrency)",
  "event GemMined(uint256 indexed gemId,address owner,uint256 promisLoad)",
];
const ERC20_ABI = [
  "function balanceOf(address) view returns (uint256)",
  "function decimals() view returns (uint8)",
];
const PROMIS_ABI = [
  "function balanceOf(address) view returns (bytes)",
  "function opNonceOf(address) view returns (uint64)",
  "function decimals() view returns (uint8)",
];
const NATIVE_ABI = [
  "function mineRudis(uint256,bytes32,uint64) returns (uint256)",
  "event RudisMined(address indexed sender,uint256 amount)",
];

function requireThat(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

export function parseGemId(value: string): bigint {
  requireThat(/^(0x[0-9a-fA-F]+|[0-9]+)$/.test(value), "GEM ID must be decimal or 0x hex");
  const id = BigInt(value);
  requireThat(id > 0n && id <= ethers.MaxUint256, "GEM ID must fit a positive uint256");
  return id;
}

export function assertSettleable(gem: { state: bigint; calledAt: bigint; callNoticePeriod: bigint }, timestamp: bigint) {
  requireThat(gem.state === 1n || gem.state === 2n, `GEM is ${STATES[Number(gem.state)] ?? "unknown"}; settlement requires Qualified or Called`);
  if (gem.state === 2n) {
    requireThat(timestamp <= gem.calledAt + gem.callNoticePeriod, "GEM settlement deadline expired");
  }
}

// Persist before broadcasting, including the signed bytes. On a timeout/restart
// the identical transaction is retried, never a second payment with a new nonce.
interface SavedTx { raw: string; hash: string }
interface JournalData {
  version: number; chainId: string; owner: string; gemId: string; amount: string;
  transactions: Record<string, SavedTx>;
}
export interface Receipt {
  status: number | null;
  logs: readonly { address: string; topics: readonly string[]; data: string }[];
}
export interface Transport {
  receipt(hash: string): Promise<Receipt | null>;
  broadcast(raw: string): Promise<unknown>;
  wait(hash: string): Promise<Receipt | null>;
}

function durableJson(path: string, data: unknown) {
  const temporary = `${path}.tmp`;
  const fd = openSync(temporary, "w", 0o600);
  try {
    writeFileSync(fd, JSON.stringify(data, null, 2) + "\n");
    fsyncSync(fd);
  } finally { closeSync(fd); }
  renameSync(temporary, path);
  const dir = openSync(resolve(path, ".."), "r");
  try { fsyncSync(dir); } finally { closeSync(dir); }
}

export class Journal {
  data: JournalData;
  constructor(readonly path: string, identity: Omit<JournalData, "transactions" | "version">) {
    this.data = existsSync(path) ? JSON.parse(readFileSync(path, "utf8")) :
      { version: 1, ...identity, transactions: {} };
    requireThat(this.data.version === 1 && Object.entries(identity).every(([k, v]) =>
      this.data[k as keyof JournalData] === v), "Saved operation does not match chain/wallet/GEM/amount");
    this.save();
  }
  save() { durableJson(this.path, this.data); }
  async send(step: string, prepare: () => Promise<string>, transport: Transport): Promise<Receipt> {
    let tx = this.data.transactions[step];
    if (!tx) {
      const raw = await prepare();
      tx = { raw, hash: ethers.keccak256(raw) };
      this.data.transactions[step] = tx;
      this.save();
    }
    console.log(`${step}: ${tx.hash}`);
    let receipt = await transport.receipt(tx.hash);
    if (!receipt) {
      // A node can reject re-broadcast as "already known". In either case wait
      // for this exact hash; an unresolved hash must not advance the workflow.
      try { await transport.broadcast(tx.raw); }
      catch { console.error(`${step}: broadcast not acknowledged; checking saved hash`); }
      receipt = await transport.wait(tx.hash);
    }
    requireThat(receipt, `${step}: receipt not confirmed; rerun the same command to resume`);
    if (receipt.status === 0) {
      delete this.data.transactions[step];
      this.save();
      throw new Error(`${step}: transaction reverted (${tx.hash}); subsequent steps were not sent`);
    }
    requireThat(receipt.status === 1, `${step}: receipt has no successful status`);
    return receipt;
  }
}

function event(receipt: Receipt, address: string, abi: string[], name: string) {
  const iface = new ethers.Interface(abi);
  for (const log of receipt.logs) {
    if (log.address.toLowerCase() !== address.toLowerCase()) continue;
    try {
      const parsed = iface.parseLog(log);
      if (parsed?.name === name) return parsed.args;
    } catch { /* Other events from the same precompile. */ }
  }
  throw new Error(`Successful receipt is missing ${name}; stopped for inspection`);
}

// Do not propagate exec errors: they can embed argv containing the private key.
async function runCli(binary: string, args: string[], cwd: string, privateKey?: string): Promise<string> {
  return new Promise((resolveOutput, reject) => {
    const child = spawn(binary, args, { cwd, stdio: ["ignore", "pipe", "pipe"], shell: false });
    let stdout = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => { stdout += chunk; });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => { stderr += chunk; });
    child.on("error", () => reject(new Error("Cannot launch PayNote CLI; set --outbe-cli to a built outbe-cli binary")));
    child.on("close", (code) => {
      // The CLI emits note paths and transaction hashes here, never note JSON.
      if (stderr) console.error(privateKey ? stderr.split(privateKey).join("[REDACTED]").trim() : stderr.trim());
      if (code === 0) resolveOutput(stdout);
      else reject(new Error(`PayNote CLI failed (exit ${code}); saved notes remain in ${cwd}`));
    });
  });
}

async function paymentProof(binary: string, rpc: string, wallet: ethers.Wallet, dir: string,
  amount: bigint, token: ethers.Contract, pool: ethers.Contract): Promise<string> {
  requireThat(amount > 0n, "Settlement quote must be positive");
  const notesDir = join(dir, "paynotes");
  const depositPath = join(dir, "deposit-started.json");
  const noteFiles = () => existsSync(notesDir) ? readdirSync(notesDir).filter(n => /^0x[0-9a-f]{64}\.json$/.test(n)) : [];
  let files = noteFiles();
  if (!files.length) {
    requireThat(!existsSync(depositPath), "Previous deposit was interrupted. Inspect its receipt and saved notes before retrying; no second deposit was sent");
    requireThat(await token.balanceOf(wallet.address) >= amount, "Insufficient WUSDC for settlement");
    // This marker prevents a duplicate deposit even if the CLI stops before
    // returning its JSON result. Its own note file is saved before approval.
    durableJson(depositPath, { amount: amount.toString() });
    await runCli(binary, ["--rpc-url", rpc, "--private-key", wallet.privateKey,
      "paynote", "deposit", WUSDC, amount.toString()], dir, wallet.privateKey);
    files = noteFiles();
  }
  const deposit = existsSync(depositPath) ? JSON.parse(readFileSync(depositPath, "utf8")) : {};
  if (!deposit.originalNote) {
    requireThat(files.length === 1, "Expected exactly one original PayNote; inspect the operation directory");
    deposit.originalNote = files[0];
    // Remember the source before proving: a lower quote creates a second,
    // initially unspendable change note in the same directory.
    durableJson(depositPath, deposit);
  }
  requireThat(files.includes(deposit.originalNote), "Original PayNote file is missing; no additional WUSDC deposited");
  const notePath = join(notesDir, deposit.originalNote);
  const note = JSON.parse(readFileSync(notePath, "utf8"));
  requireThat(BigInt(note.chain_id) === CHAIN_ID && ethers.getAddress(note.asset) === WUSDC &&
    ethers.getAddress(note.pool) === PAYNOTE && BigInt(note.amount) >= amount,
  "Saved PayNote does not cover this chain/asset/quote; no additional WUSDC deposited");
  requireThat(await pool.hasCommitment(note.commitment), "Saved PayNote deposit is not confirmed; inspect the deposit transaction before retrying");
  const proof = JSON.parse(await runCli(binary, ["--rpc-url", rpc, "paynote", "spend-proof",
    notePath, amount.toString(), "--owner", wallet.address], dir));
  requireThat(BigInt(proof.chain_id) === CHAIN_ID && ethers.getAddress(proof.owner) === wallet.address &&
    ethers.getAddress(proof.asset) === WUSDC && BigInt(proof.spend_amount) === amount &&
    proof.source_commitment === note.commitment && ethers.isHexString(proof.proof),
  "PayNote proof output does not match this settlement");
  return proof.proof;
}

const HELP = `Rudis GEM wallet (Rehearsal, chain 70860602)
  npm run rudis-gems -- list --private-key "$PK"
  npm run rudis-gems -- list --address 0x...
  npm run rudis-gems -- settle --gem-id 0x... --private-key "$PK"

Options:
  --rpc-url URL          Default: https://125.253.92.5
  --outbe-cli PATH       Rust CLI with paynote deposit/spend-proof support
  --max-settlement N     Maximum WUSDC payment, in whole-token decimal units
  --state-dir PATH       Default: <repo>/.rudis-gems (keep for recovery)
  --dry-run             Show settlement plan without submitting transactions
  --help                Show this help

settle pays WUSDC via PayNote, mines this GEM's PROMIS, then converts exactly
that amount to native RUDIS (COEN). Rerun the same command to resume.
`;

export async function main(argv: string[]) {
  const { values, positionals } = parseArgs({ args: argv, allowPositionals: true, options: {
    "private-key": { type: "string" }, address: { type: "string" }, "gem-id": { type: "string" },
    "rpc-url": { type: "string", default: "https://125.253.92.5" },
    "outbe-cli": { type: "string" }, "state-dir": { type: "string", default: join(ROOT, ".rudis-gems") },
    "max-settlement": { type: "string" }, "dry-run": { type: "boolean" }, help: { type: "boolean" },
  } });
  if (values.help) { console.log(HELP); return; }
  const command = positionals[0];
  requireThat(positionals.length === 1 && (command === "list" || command === "settle"), HELP);
  requireThat(!(values.address && values["private-key"]), "Specify either --address or --private-key");
  requireThat(command !== "settle" || (!!values["private-key"] && !!values["gem-id"]), "settle requires --private-key and --gem-id");
  requireThat(command !== "list" || !values["gem-id"], "--gem-id is only used with settle");
  const id = values["gem-id"] ? parseGemId(values["gem-id"]) : undefined;
  let wallet: ethers.Wallet | undefined;
  try { if (values["private-key"]) wallet = new ethers.Wallet(values["private-key"]); }
  catch { throw new Error("Invalid --private-key (expected a secp256k1 private key)"); }
  const owner = wallet?.address ?? (values.address ? ethers.getAddress(values.address) : undefined);
  requireThat(owner, "Specify --private-key or --address");
  const provider = new ethers.JsonRpcProvider(values["rpc-url"], undefined, { cacheTimeout: -1 });
  provider.pollingInterval = 1500;
  try {
    const network = await provider.getNetwork();
    requireThat(network.chainId === CHAIN_ID, `Wrong network: expected ${CHAIN_ID}, got ${network.chainId}`);
    const gem = new ethers.Contract(GEM, GEM_ABI, provider);
    const factory = new ethers.Contract(FACTORY, FACTORY_ABI, provider);
    const token = new ethers.Contract(WUSDC, ERC20_ABI, provider);
    const promis = new ethers.Contract(PROMIS, PROMIS_ABI, provider);
    const decimals = await token.decimals();
    requireThat(await promis.decimals() === 6n, "Unsupported PROMIS denomination (expected 6 decimals)");
    const block = await provider.getBlock("latest");
    requireThat(block, "Cannot read latest block");
    const at = { blockTag: block.number };
    const [balance, native] = await Promise.all([token.balanceOf(owner, at), provider.getBalance(owner, block.number)]);
    console.log(`Wallet: ${owner}\nNetwork: Rudis ${CHAIN_ID}, block ${block.number}`);
    console.log(`WUSDC: ${ethers.formatUnits(balance, decimals)}\nRUDIS (COEN): ${ethers.formatEther(native)}`);
    if (command === "list") {
      const count: bigint = await gem.balanceOf(owner, at);
      console.log(`GEMs: ${count}\nGEM ID\tPROMIS\tSettlement WUSDC\tState`);
      for (let i = 0n; i < count; i++) {
        const gemId: bigint = await gem.tokenOfOwnerByIndex(owner, i, at);
        const item = await gem.getGemStatus(gemId, at);
        let cost = "0 (paid)";
        if (item.state !== 3n) {
          try { cost = ethers.formatUnits((await factory.quoteSettlement(gemId, WUSDC, at))[1], decimals); }
          catch (error) {
            if (!ethers.isError(error, "CALL_EXCEPTION")) throw error;
            cost = `unavailable: ${error.reason ?? "quote reverted"}`;
          }
        }
        const expired = item.state === 2n && BigInt(block.timestamp) > item.calledAt + item.callNoticePeriod;
        console.log(`${ethers.toBeHex(gemId, 32)}\t${ethers.formatUnits(item.promisLoad, 6)}\t${cost}\t${expired ? "Called (expired)" : STATES[Number(item.state)] ?? "unknown"}`);
      }
      return;
    }
    requireThat(wallet && id !== undefined, "Missing signer/GEM");
    wallet = wallet.connect(provider);
    const signer = wallet;
    const dir = join(resolve(values["state-dir"]!), CHAIN_ID.toString(), owner.toLowerCase(), ethers.toBeHex(id, 32));
    const journalPath = join(dir, "operation.json");
    const previous: JournalData | undefined = existsSync(journalPath) ? JSON.parse(readFileSync(journalPath, "utf8")) : undefined;
    // After minePromis the GEM is burned. Its successful saved receipt is the
    // evidence used for resuming conversion, rather than querying a missing NFT.
    const item = previous?.transactions.mine ? undefined : await gem.getGemStatus(id);
    if (item) requireThat(ethers.getAddress(item.owner) === owner, "This GEM belongs to another wallet");
    const amount: bigint = item?.promisLoad ?? BigInt(previous!.amount);
    requireThat(amount > 0n, "GEM has zero PROMIS load");
    let quote = 0n;
    if (item && item.state !== 3n) {
      assertSettleable(item, BigInt(block.timestamp));
      quote = (await factory.quoteSettlement(id, WUSDC))[1];
    }
    const cap = values["max-settlement"] === undefined ? undefined : ethers.parseUnits(values["max-settlement"], decimals);
    requireThat(cap === undefined || (cap >= 0n && quote <= cap), "Settlement quote exceeds --max-settlement");
    console.log(`GEM: ${ethers.toBeHex(id, 32)}\nPROMIS -> RUDIS: ${ethers.formatUnits(amount, 6)}\nSettlement WUSDC: ${ethers.formatUnits(quote, decimals)}`);
    if (values["dry-run"]) return;
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    let lock: number;
    try { lock = openSync(join(dir, "run.lock"), "wx", 0o600); }
    catch { throw new Error(`Operation is locked: ${dir}/run.lock. If the previous process stopped, remove only run.lock and rerun`); }
    try {
      const journal = new Journal(journalPath, { chainId: CHAIN_ID.toString(), owner, gemId: id.toString(), amount: amount.toString() });
      const transport: Transport = {
        receipt: hash => provider.getTransactionReceipt(hash),
        broadcast: raw => provider.broadcastTransaction(raw),
        wait: hash => provider.waitForTransaction(hash, 1, 120_000),
      };
      const prepare = async (address: string, abi: string[], method: string, args: unknown[]) => {
        const data = new ethers.Interface(abi).encodeFunctionData(method, args);
        const gas = await provider.estimateGas({ from: owner, to: address, data });
        return signer.signTransaction(await signer.populateTransaction({ to: address, data, gasLimit: gas * 12n / 10n }));
      };
      // Derive/decrypt before depositing money, so an unavailable TEE fails early.
      const keys = await deriveKeys(signer, "Promis", "rudis_deriveKeys");
      const promisBalance = async () => decryptBalance(keys.viewKey, owner, await promis.balanceOf(owner), "Promis");
      await promisBalance();
      if (journal.data.transactions.settle || (item && item.state !== 3n)) {
        const receipt = await journal.send("settle", async () => {
          const binaryOption = values["outbe-cli"];
          const binary = binaryOption ? (binaryOption.includes("/") ? resolve(binaryOption) : binaryOption) :
            existsSync(join(ROOT, "target/release/outbe-cli")) ? join(ROOT, "target/release/outbe-cli") : "outbe-cli";
          await runCli(binary, ["paynote", "spend-proof", "--help"], dir);
          const pool = new ethers.Contract(PAYNOTE, ["function hasCommitment(bytes32) view returns (bool)"], provider);
          const proof = await paymentProof(binary, values["rpc-url"]!, signer, dir, quote, token, pool);
          const latest = await provider.getBlock("latest");
          requireThat(latest, "Cannot recheck settlement deadline");
          assertSettleable(await gem.getGemStatus(id), BigInt(latest.timestamp));
          requireThat((await factory.quoteSettlement(id, WUSDC))[1] === quote, "Settlement quote changed; rerun to regenerate the proof from the saved note");
          return prepare(FACTORY, FACTORY_ABI, "settleGem", [id, proof]);
        }, transport);
        const settled = event(receipt, FACTORY, FACTORY_ABI, "GemSettled");
        requireThat(settled.gemId === id && ethers.getAddress(settled.owner) === owner, "Settlement receipt belongs to another GEM/owner");
      }
      const mintReceipt = await journal.send("mine", async () => {
        const current = await gem.getGemStatus(id);
        requireThat(current.state === 3n && current.promisLoad === amount && ethers.getAddress(current.owner) === owner,
          "GEM is not settled with the expected owner/load");
        const nonce: bigint = await promis.opNonceOf(owner);
        const mac = modifyMac(keys.modifyKey, owner, PromisOp.Mint, amount, nonce, CHAIN_ID, "Promis");
        return prepare(FACTORY, FACTORY_ABI, "minePromis", [id, findPowNonce(id), mac, nonce]);
      }, transport);
      const mined = event(mintReceipt, FACTORY, FACTORY_ABI, "GemMined");
      requireThat(mined.gemId === id && ethers.getAddress(mined.owner) === owner && mined.promisLoad === amount, "Unexpected GEM mint receipt");
      const conversionReceipt = await journal.send("convert", async () => {
        requireThat(await promisBalance() >= amount, "Insufficient PROMIS to convert this GEM's amount");
        const nonce: bigint = await promis.opNonceOf(owner); // fresh after mint
        const mac = modifyMac(keys.modifyKey, owner, PromisOp.Burn, amount, nonce, CHAIN_ID, "Promis");
        return prepare(PROMIS_FACTORY, NATIVE_ABI, "mineRudis", [amount, mac, nonce]);
      }, transport);
      const converted = event(conversionReceipt, PROMIS_FACTORY, NATIVE_ABI, "RudisMined");
      requireThat(ethers.getAddress(converted.sender) === owner && converted.amount === amount * NATIVE_PER_PROMIS_MINOR,
        "Unexpected RUDIS conversion receipt");
      console.log(`Done: ${ethers.formatUnits(amount, 6)} PROMIS converted to RUDIS (COEN)`);
      console.log(`WUSDC: ${ethers.formatUnits(await token.balanceOf(owner), decimals)}\nPROMIS: ${ethers.formatUnits(await promisBalance(), 6)}\nRUDIS: ${ethers.formatEther(await provider.getBalance(owner))}`);
      console.log(`Recovery files: ${dir}`);
    } finally {
      closeSync(lock);
      unlinkSync(join(dir, "run.lock"));
    }
  } finally { provider.destroy(); }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).catch(error => {
    // Ethers' complete errors may include transaction data or input key values.
    const keyIndex = process.argv.indexOf("--private-key");
    const key = keyIndex >= 0 ? process.argv[keyIndex + 1] : undefined;
    const message = ethers.isError(error, "CALL_EXCEPTION") ? error.reason ?? "Contract call reverted" :
      ethers.isError(error, "UNKNOWN_ERROR") ? "RPC rejected the request; inspect the saved transaction hash" :
      error instanceof Error ? error.message : "Operation failed";
    console.error(`Error: ${key ? message.split(key).join("[REDACTED]") : message}`);
    process.exitCode = 1;
  });
}
