import { ethers, Wallet } from "ethers";
import {
  IGratis__factory,
  IGratisFactory__factory,
  IGratisPool__factory,
} from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_GRATIS_FACTORY_ADDRESS,
  DEFAULT_GRATIS_POOL_ADDRESS,
  GRATIS_DENOMINATIONS,
  formatToken,
  formatTokenDiff,
  fetchTokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv,
} from "./utils.js";
import {
  ACTION_UNPLEDGE,
  buildMerkleProof,
  fieldToHex32,
  nullifierHash,
  proveUnpledge,
  receiverBinding,
} from "./shielded.js";
import { deleteTicket, findLatestTicket, readTicket, type Ticket } from "./ticket.js";

// Parse CLI args: [ticketPath?] [envName?]
let ticketPath: string | undefined;
let envName = DEFAULT_ENV;

const args = process.argv.slice(2);
for (const a of args) {
  if (a.endsWith(".json")) ticketPath = a;
  else envName = a;
}

const { envPath } = loadEnv(import.meta.url, envName);

const rpcUrl = requireEnv("RPC_URL", envPath);
const userPrivateKey = requireEnv("USER_PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const gratisAddress = process.env["GRATIS_ADDRESS"] || DEFAULT_GRATIS_ADDRESS;
const gratisFactoryAddress = process.env["GRATIS_FACTORY_ADDRESS"] || DEFAULT_GRATIS_FACTORY_ADDRESS;
const gratisPoolAddress = process.env["GRATIS_POOL_ADDRESS"] || DEFAULT_GRATIS_POOL_ADDRESS;

function loadTicket(): { ticket: Ticket; path: string } {
  if (ticketPath) {
    return { ticket: readTicket(ticketPath), path: ticketPath };
  }
  const latest = findLatestTicket();
  if (!latest) {
    console.error(
      "No ticket file found under examples/credis-flow/tickets/. Run `npm run pledge-gratis` first or pass an explicit ticket path.",
    );
    process.exit(1);
  }
  return latest;
}

async function main() {
  const { ticket, path: usedTicketPath } = loadTicket();
  const denom = GRATIS_DENOMINATIONS.find((d) => d.id === ticket.denomId);
  if (!denom) {
    throw new Error(`Ticket denomId ${ticket.denomId} not in known ladder`);
  }

  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const wallet = new Wallet(userPrivateKey, provider);
  const gratis = IGratis__factory.connect(gratisAddress, wallet);
  const gratisFactory = IGratisFactory__factory.connect(gratisFactoryAddress, wallet);
  const gratisPool = IGratisPool__factory.connect(gratisPoolAddress, provider);

  const [gratisMeta, network] = await Promise.all([
    fetchTokenMeta(gratis),
    provider.getNetwork(),
  ]);

  const secret = BigInt(ticket.secret);
  const nullifierSecret = BigInt(ticket.nullifierSecret);
  const commitment = BigInt(ticket.commitment);
  const denomId = ticket.denomId;
  const leafIndex = ticket.leafIndex;

  console.log("=== Unpledge Gratis (shielded) ===");
  console.log(`Env:           ${envName} (${envPath})`);
  console.log(`RPC:           ${rpcUrl}`);
  console.log(`Ticket:        ${usedTicketPath}`);
  console.log(`User:          ${userAddress}`);
  console.log(`Factory:       ${gratisFactoryAddress}`);
  console.log(`Pool:          ${gratisPoolAddress}`);
  console.log(`Denom:         ${denomId} = ${formatToken(denom.amount, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`Commitment:    ${fieldToHex32(commitment)}`);
  console.log(`Leaf index:    ${leafIndex}`);
  console.log(`Chain ID:      ${network.chainId}`);

  // Pre-flight: catch the easy failure modes before paying for proof generation.
  const nullifier = await nullifierHash(nullifierSecret);
  const [onChainRoot, onChainLeafCount, alreadySpent] = await Promise.all([
    gratisPool.currentRoot(denomId),
    gratisPool.leafCount(denomId),
    gratisPool.isSpent(nullifier),
  ]);

  if (Number(onChainLeafCount) <= leafIndex) {
    throw new Error(
      `Pool leafCount ${onChainLeafCount} <= ticket.leafIndex ${leafIndex} — ticket points past the tree`,
    );
  }
  if (alreadySpent) {
    deleteTicket(usedTicketPath);
    throw new Error(
      `Nullifier ${fieldToHex32(nullifier)} is already spent on-chain — ticket has been deleted (cannot be unpledged a second time)`,
    );
  }

  // Rebuild the local Merkle tree from the on-chain CommitmentInserted log.
  console.log("\nReplaying CommitmentInserted events to rebuild the Merkle tree...");
  const filter = gratisPool.filters["CommitmentInserted(uint8,uint256,uint32,uint256)"](denomId);
  const events = await gratisPool.queryFilter(filter, 0, "latest");

  const commitments: bigint[] = [];
  for (const ev of events) {
    const idx = Number(ev.args.leafIndex);
    commitments[idx] = BigInt(ev.args.commitment);
  }
  for (let i = 0; i < commitments.length; i++) {
    if (commitments[i] === undefined) {
      throw new Error(`Missing CommitmentInserted event for leaf ${i} of denom ${denomId}`);
    }
  }
  console.log(`  Replayed ${commitments.length} commitments for denom ${denomId}`);

  if (commitments[leafIndex] !== commitment) {
    throw new Error(
      `Replayed commitment[${leafIndex}] = ${fieldToHex32(commitments[leafIndex])} does not match ticket commitment ${fieldToHex32(commitment)}`,
    );
  }

  const merkle = await buildMerkleProof(commitments, leafIndex);
  if (merkle.root !== BigInt(onChainRoot)) {
    throw new Error(
      `Locally-rebuilt root ${fieldToHex32(merkle.root)} does not match on-chain currentRoot ${fieldToHex32(BigInt(onChainRoot))} — likely Poseidon param drift between off-chain and on-chain implementations.`,
    );
  }
  console.log(`  Rebuilt root matches on-chain currentRoot: ${fieldToHex32(merkle.root)}`);

  // Compute receiver binding. `target = msg.sender` (the user wallet) and
  // `nonce = 0` for the terminal unpledge action. Matches state.rs::receiver_binding.
  const binding = await receiverBinding(
    ACTION_UNPLEDGE,
    userAddress,
    network.chainId,
    0n,
  );

  console.log("\nGenerating UltraHonkKeccak proof (this typically takes 3-8s)...");
  const tProveStart = Date.now();
  const proof = await proveUnpledge({
    secret,
    nullifierSecret,
    denomId,
    merklePath: merkle.siblings,
    merkleIndex: leafIndex,
    merkleRoot: merkle.root,
    nullifierHashValue: nullifier,
    receiverBindingValue: binding,
  });
  console.log(`  Proof generated in ${((Date.now() - tProveStart) / 1000).toFixed(2)}s (${proof.length} bytes)`);

  const [balanceBefore, pledgedBefore, pledgedTotalBefore] = await Promise.all([
    gratis.balanceOf(userAddress),
    gratis.pledgedOf(userAddress),
    gratis.pledgedTotalSupply(),
  ]);

  console.log("\n=== State BEFORE ===");
  console.log(`  User balance:    ${formatToken(balanceBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  User pledged:    ${formatToken(pledgedBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged total:   ${formatToken(pledgedTotalBefore, gratisMeta.decimals, gratisMeta.symbol)}`);

  console.log("\nSending unpledgeGratis tx...");
  const tx = await gratisFactory.unpledgeGratis({
    merkleRoot: merkle.root,
    nullifierHash: nullifier,
    denomId,
    receiverBinding: binding,
    proof: ethers.hexlify(proof),
  });
  console.log(`  TX hash: ${tx.hash}`);
  const receipt = await tx.wait();
  if (!receipt) throw new Error("unpledgeGratis tx receipt missing");
  console.log(`  Block:   ${receipt.blockNumber}`);
  console.log(`  Gas:     ${receipt.gasUsed}`);

  // Decode interesting events from factory + pool.
  const factoryIface = IGratisFactory__factory.createInterface();
  const poolIface = IGratisPool__factory.createInterface();
  console.log("\n=== Events ===");
  for (const log of receipt.logs) {
    const iface =
      log.address.toLowerCase() === gratisFactoryAddress.toLowerCase()
        ? factoryIface
        : log.address.toLowerCase() === gratisPoolAddress.toLowerCase()
          ? poolIface
          : null;
    if (!iface) continue;
    try {
      const parsed = iface.parseLog({ topics: log.topics as string[], data: log.data });
      if (parsed) {
        const argsObj: Record<string, string> = {};
        parsed.fragment.inputs.forEach((p, i) => {
          argsObj[p.name] = String(parsed.args[i]);
        });
        console.log(`  ${parsed.name}: ${JSON.stringify(argsObj)}`);
      }
    } catch {
      // not our event
    }
  }

  const [balanceAfter, pledgedAfter, pledgedTotalAfter, spentAfter] = await Promise.all([
    gratis.balanceOf(userAddress),
    gratis.pledgedOf(userAddress),
    gratis.pledgedTotalSupply(),
    gratisPool.isSpent(nullifier),
  ]);

  console.log("\n=== State AFTER ===");
  console.log(`  User balance:    ${formatToken(balanceAfter, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  User pledged:    ${formatToken(pledgedAfter, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged total:   ${formatToken(pledgedTotalAfter, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Nullifier spent: ${spentAfter}`);

  console.log("\n=== CHANGES ===");
  console.log(`  Balance:  ${formatTokenDiff(balanceAfter - balanceBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged:  ${formatTokenDiff(pledgedAfter - pledgedBefore, gratisMeta.decimals, gratisMeta.symbol)}`);

  deleteTicket(usedTicketPath);
  console.log(`\nTicket deleted: ${usedTicketPath} (nullifier is on-chain; the secret has no further use).`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
