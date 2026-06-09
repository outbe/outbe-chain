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
  commitmentHash,
  toField,
  fieldToHex32,
} from "./shielded.js";
import { writeTicket } from "./ticket.js";

// Parse CLI args: [denomId] [envName]
// denomId defaults to 1 (the smallest valid denomination); id 0 is
// intentionally invalid on-chain — see `utils.ts::GRATIS_DENOMINATIONS`.
const denomId = process.argv[2] !== undefined ? Number(process.argv[2]) : 1;
const envName = process.argv[3] || DEFAULT_ENV;

const denom = (() => {
  const d = GRATIS_DENOMINATIONS.find((x) => x.id === denomId);
  if (!d) {
    console.error(
      `Unknown denomId ${denomId}. Valid: ${GRATIS_DENOMINATIONS.map((x) => x.id).join(", ")}`,
    );
    process.exit(1);
  }
  return d;
})();

const { envPath } = loadEnv(import.meta.url, envName);

const rpcUrl = requireEnv("RPC_URL", envPath);
const userPrivateKey = requireEnv("USER_PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const gratisAddress = process.env["GRATIS_ADDRESS"] || DEFAULT_GRATIS_ADDRESS;
const gratisFactoryAddress = process.env["GRATIS_FACTORY_ADDRESS"] || DEFAULT_GRATIS_FACTORY_ADDRESS;
const gratisPoolAddress = process.env["GRATIS_POOL_ADDRESS"] || DEFAULT_GRATIS_POOL_ADDRESS;

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const wallet = new Wallet(userPrivateKey, provider);
  const gratis = IGratis__factory.connect(gratisAddress, wallet);
  const gratisFactory = IGratisFactory__factory.connect(gratisFactoryAddress, wallet);
  const gratisPool = IGratisPool__factory.connect(gratisPoolAddress, provider);

  const gratisMeta = await fetchTokenMeta(gratis);

  // Fresh 32-byte secrets reduced into the BN254 scalar field. This matches the
  // on-chain `state.rs::u256_to_fr` reduction.
  const secret = toField(ethers.getBytes(ethers.hexlify(ethers.randomBytes(32))));
  const nullifierSecret = toField(ethers.getBytes(ethers.hexlify(ethers.randomBytes(32))));
  const commitment = await commitmentHash(secret, nullifierSecret, denomId);

  console.log("=== Pledge Gratis (shielded) ===");
  console.log(`Env:           ${envName} (${envPath})`);
  console.log(`RPC:           ${rpcUrl}`);
  console.log(`User:          ${userAddress}`);
  console.log(`Gratis:        ${gratisAddress} (${gratisMeta.symbol}, ${gratisMeta.decimals} decimals)`);
  console.log(`Factory:       ${gratisFactoryAddress}`);
  console.log(`Pool:          ${gratisPoolAddress}`);
  console.log(`Denom:         ${denomId} = ${formatToken(denom.amount, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`Secret:        ${fieldToHex32(secret)}`);
  console.log(`NullifierSecr: ${fieldToHex32(nullifierSecret)}`);
  console.log(`Commitment:    ${fieldToHex32(commitment)}`);

  const gratisBalance = await gratis.balanceOf(userAddress);
  if (gratisBalance < denom.amount) {
    console.error(
      `Insufficient Gratis balance: have ${formatToken(gratisBalance, gratisMeta.decimals, gratisMeta.symbol)}, need ${formatToken(denom.amount, gratisMeta.decimals, gratisMeta.symbol)}`,
    );
    process.exit(1);
  }

  const [balanceBefore, pledgedBefore, pledgedTotalBefore, leafCountBefore, rootBefore] =
    await Promise.all([
      gratis.balanceOf(userAddress),
      gratis.pledgedOf(userAddress),
      gratis.pledgedTotalSupply(),
      gratisPool.leafCount(denomId),
      gratisPool.currentRoot(denomId),
    ]);

  console.log("\n=== State BEFORE ===");
  console.log(`  User balance:    ${formatToken(balanceBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  User pledged:    ${formatToken(pledgedBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged total:   ${formatToken(pledgedTotalBefore, gratisMeta.decimals, gratisMeta.symbol)} (system-wide)`);
  console.log(`  Pool leafCount:  ${leafCountBefore} (denom ${denomId})`);
  console.log(`  Pool root:       ${fieldToHex32(rootBefore)}`);

  console.log("\nSending pledgeGratis tx...");
  const tx = await gratisFactory.pledgeGratis(denomId, commitment);
  console.log(`  TX hash: ${tx.hash}`);
  const receipt = await tx.wait();
  if (!receipt) throw new Error("pledgeGratis tx receipt missing");
  console.log(`  Block:   ${receipt.blockNumber}`);
  console.log(`  Gas:     ${receipt.gasUsed}`);

  // Decode the CommitmentInserted event from the pool to capture leafIndex + new root.
  const poolIface = IGratisPool__factory.createInterface();
  const inserted = receipt.logs
    .filter((l) => l.address.toLowerCase() === gratisPoolAddress.toLowerCase())
    .map((l) => {
      try {
        return poolIface.parseLog({ topics: l.topics as string[], data: l.data });
      } catch {
        return null;
      }
    })
    .find((p) => p?.name === "CommitmentInserted");

  if (!inserted) {
    throw new Error("CommitmentInserted event not found in receipt");
  }

  const eventCommitment = BigInt(inserted.args.commitment);
  const eventLeafIndex = Number(inserted.args.leafIndex);
  const eventRoot = BigInt(inserted.args.newRoot);

  if (eventCommitment !== commitment) {
    throw new Error(
      `Poseidon parity mismatch: sent commitment ${fieldToHex32(commitment)} but on-chain event recorded ${fieldToHex32(eventCommitment)}. Off-chain Poseidon does not agree with the runtime — abort before any unpledge would silently fail.`,
    );
  }

  const [balanceAfter, pledgedAfter, pledgedTotalAfter, leafCountAfter] =
    await Promise.all([
      gratis.balanceOf(userAddress),
      gratis.pledgedOf(userAddress),
      gratis.pledgedTotalSupply(),
      gratisPool.leafCount(denomId),
    ]);

  console.log("\n=== State AFTER ===");
  console.log(`  User balance:    ${formatToken(balanceAfter, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  User pledged:    ${formatToken(pledgedAfter, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged total:   ${formatToken(pledgedTotalAfter, gratisMeta.decimals, gratisMeta.symbol)} (system-wide)`);
  console.log(`  Pool leafCount:  ${leafCountAfter} (denom ${denomId})`);
  console.log(`  Pool root:       ${fieldToHex32(eventRoot)}`);
  console.log(`  Leaf index:      ${eventLeafIndex}`);

  console.log("\n=== CHANGES ===");
  console.log(`  Balance:  ${formatTokenDiff(balanceAfter - balanceBefore, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged:  ${formatTokenDiff(pledgedAfter - pledgedBefore, gratisMeta.decimals, gratisMeta.symbol)}`);

  const network = await provider.getNetwork();
  const ticketPath = writeTicket({
    denomId,
    secret: fieldToHex32(secret),
    nullifierSecret: fieldToHex32(nullifierSecret),
    commitment: fieldToHex32(commitment),
    leafIndex: eventLeafIndex,
    root: fieldToHex32(eventRoot),
    blockNumber: receipt.blockNumber,
    txHash: receipt.hash,
    chainId: network.chainId.toString(),
    createdAt: new Date().toISOString(),
  });

  console.log(`\nTicket written: ${ticketPath}`);
  console.log("Run `npm run unpledge-gratis-fast` to spend this commitment.");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
