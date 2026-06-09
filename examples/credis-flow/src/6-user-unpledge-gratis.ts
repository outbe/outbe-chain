import { ethers, Wallet } from "ethers";
import { IGratis__factory, IGratisFactory__factory } from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_GRATIS_FACTORY_ADDRESS,
  DEFAULT_CREDIS_ADDRESS,
  formatToken,
  formatTokenMeta,
  formatTokenDiff,
  fetchTokenMeta,
  TokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv,
} from "./utils.js";

// Parse CLI args: [secret] [envName]
const secret = process.argv[2]
  ? BigInt(process.argv[2])
  : (() => { throw new Error("secret is required as 1st argument"); })();
const envName = process.argv[3] || DEFAULT_ENV;

// Load env
const { envPath } = loadEnv(import.meta.url, envName);

const rpcUrl = requireEnv("RPC_URL", envPath);
const userPrivateKey = requireEnv("USER_PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const gratisAddress = process.env["GRATIS_ADDRESS"] || DEFAULT_GRATIS_ADDRESS;
const gratisFactoryAddress = process.env["GRATIS_FACTORY_ADDRESS"] || DEFAULT_GRATIS_FACTORY_ADDRESS;
const credisAddress = process.env["CREDIS_ADDRESS"] || DEFAULT_CREDIS_ADDRESS;

interface State {
  userGratisBalance: bigint;
  userPledged: bigint;
  credisGratisBalance: bigint;
  pledgeTickets: Awaited<ReturnType<ReturnType<typeof IGratisFactory__factory.connect>["getPledgeTicketByAddress"]>>;
}

async function getState(
  gratis: ReturnType<typeof IGratis__factory.connect>,
  gratisFactory: ReturnType<typeof IGratisFactory__factory.connect>,
): Promise<State> {
  const [userGratisBalance, userPledged, credisGratisBalance, pledgeTickets] = await Promise.all([
    gratis.balanceOf(userAddress),
    gratis.pledgedOf(userAddress),
    gratis.balanceOf(credisAddress),
    gratisFactory.getPledgeTicketByAddress(userAddress),
  ]);
  return { userGratisBalance, userPledged, credisGratisBalance, pledgeTickets };
}

function printState(label: string, state: State, gratisMeta: TokenMeta) {
  console.log(`\n=== ${label} ===`);
  console.log(`  User (${userAddress}):`);
  console.log(`    Gratis balance: ${formatTokenMeta(state.userGratisBalance, gratisMeta)}`);
  console.log(`    Pledged:        ${formatTokenMeta(state.userPledged, gratisMeta)}`);
  console.log(`    Pledges:        ${state.pledgeTickets.length}`);
  for (const ticket of state.pledgeTickets) {
    console.log(`      - commitment: ${ticket.commitment} amount: ${formatToken(ticket.amount, gratisMeta.decimals, gratisMeta.symbol)}, block: ${ticket.createdAtBlock}`);
  }
  console.log(`  Credis (${credisAddress}):`);
  console.log(`    Gratis balance: ${formatTokenMeta(state.credisGratisBalance, gratisMeta)}`);
}

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const wallet = new Wallet(userPrivateKey, provider);
  const gratis = IGratis__factory.connect(gratisAddress, wallet);
  const gratisFactory = IGratisFactory__factory.connect(gratisFactoryAddress, wallet);

  const gratisMeta = await fetchTokenMeta(gratis);

  console.log("=== User Unlock Gratis ===");
  console.log(`Env:              ${envName} (${envPath})`);
  console.log(`RPC:              ${rpcUrl}`);
  console.log(`User:             ${userAddress}`);
  console.log(`Gratis:           ${gratisAddress} (${gratisMeta.symbol}, ${gratisMeta.decimals} decimals)`);
  console.log(`Factory:          ${gratisFactoryAddress}`);
  console.log(`Credis:           ${credisAddress}`);
  console.log(`Secret:           ${secret}`);

  // State before
  const before = await getState(gratis, gratisFactory);
  printState("State BEFORE", before, gratisMeta);

  // Unpledge (unlock) gratis
  console.log("\nSending unpledgeGratis tx...");
  const tx = await gratisFactory.unpledgeGratis(secret);
  console.log(`  TX hash: ${tx.hash}`);
  const receipt = await tx.wait();
  console.log(`  Block:   ${receipt?.blockNumber}`);
  console.log(`  Gas:     ${receipt?.gasUsed}`);

  // State after
  const after = await getState(gratis, gratisFactory);
  printState("State AFTER", after, gratisMeta);

  // Diff
  console.log("\n=== CHANGES ===");
  const balanceDiff = after.userGratisBalance - before.userGratisBalance;
  const pledgedDiff = after.userPledged - before.userPledged;
  const credisDiff = after.credisGratisBalance - before.credisGratisBalance;
  console.log(`  User balance:   ${formatTokenDiff(balanceDiff, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  User pledged:   ${formatTokenDiff(pledgedDiff, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Credis balance: ${formatTokenDiff(credisDiff, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledge tickets: ${before.pledgeTickets.length} -> ${after.pledgeTickets.length}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
