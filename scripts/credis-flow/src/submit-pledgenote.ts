import { readFileSync, writeFileSync } from "node:fs";
import { ethers, Wallet } from "ethers";
import { DEFAULT_ENV, loadEnv, requireEnv, DEFAULT_GRATIS_FACTORY_ADDRESS, DEFAULT_CREDIS_FACTORY_ADDRESS } from "./utils.js";
import { IGratisFactory__factory, ICredisFactory__factory } from "./contracts/index.js";
import { writeTicket } from "./ticket.js";

export async function submitPledgeNote(method: "createPledgeNote" | "cancelPledgeNote" | "issueCredis") {
  const path = process.argv[2];
  if (!path) throw new Error("Pass the encrypted JSON produced by outbe-cli pledge-note prepare, then an optional environment name");
  const {envPath} = loadEnv(import.meta.url, process.argv[3] || DEFAULT_ENV);
  const provider = new ethers.JsonRpcProvider(requireEnv("RPC_URL", envPath));
  const wallet = new Wallet(requireEnv(method === "issueCredis" ? "CCA_PRIVATE_KEY" : "RELAYER_PRIVATE_KEY", envPath), provider);
  const prepared = JSON.parse(readFileSync(path, "utf8")) as {to: string; data: string; value: string; read_only: boolean};
  const iface = method === "issueCredis" ? ICredisFactory__factory.createInterface() : IGratisFactory__factory.createInterface();
  const expected = method === "issueCredis" ? DEFAULT_CREDIS_FACTORY_ADDRESS : DEFAULT_GRATIS_FACTORY_ADDRESS;
  if (prepared.read_only || ethers.getAddress(prepared.to) !== ethers.getAddress(expected) || iface.parseTransaction({data: prepared.data})?.name !== method) throw new Error("Prepared transaction does not match this operation");
  if (method !== "issueCredis" && BigInt(prepared.value) !== 0n) throw new Error("Owner operations are nonpayable");
  if (method !== "issueCredis" && process.env.USER_ADDRESS?.toLowerCase() === wallet.address.toLowerCase()) throw new Error("Use an independent relayer for owner operations");
  const tx = await wallet.sendTransaction({to: prepared.to, data: prepared.data, value: BigInt(prepared.value), gasLimit: 8_000_000n});
  const receipt = await tx.wait();
  if (!receipt || receipt.status !== 1) throw new Error("PledgeNote transaction failed");
  const logs = receipt.logs.filter(log => log.address.toLowerCase() === expected.toLowerCase()).map(log => {
    try { return iface.parseLog({topics: [...log.topics], data: log.data}); } catch { return null; }
  });
  const event = logs.find(log => log?.name === (method === "issueCredis" ? "CredisIssued" : method === "createPledgeNote" ? "PledgeNoteCreated" : "PledgeNoteCancelled"));
  if (!event) throw new Error("Missing operation receipt event");
  if (method === "issueCredis") {
    const ticket = { positionId: event.args.credisId.toString(), smartAccount: event.args.smartAccount, chainId: (await provider.getNetwork()).chainId.toString(), txHash: tx.hash, createdAt: new Date().toISOString() };
    console.log(`Position receipt: ${writeTicket(ticket)}`);
  } else {
    const output = `${path}.receipt.json`;
    writeFileSync(output, JSON.stringify({encrypted_receipt: event.args.encryptedReceipt}, null, 2), {mode: 0o600, flag: "wx"});
    console.log(`Encrypted receipt: ${output}`);
  }
  console.log(`Transaction: ${tx.hash}`);
}
