import { ethers, Wallet } from "ethers";
import {
  IGratis__factory,
  ICredisFactory__factory,
  ICredis__factory,
  SmartAccountFactory__factory,
  IERC20__factory,
  IVaultProvider__factory,
} from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_CREDIS_ADDRESS,
  DEFAULT_CREDIS_FACTORY_ADDRESS,
  formatTokenMeta,
  formatTokenDiff,
  fetchTokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv,
} from "./utils.js";
import { pledgeSecret as derivePledgeSecret, spendAuth, positionId as computePositionId } from "./confidential.js";
import { findLatestTicket, readTicket, writeTicket, type Ticket } from "./ticket.js";

const SALT = 0n;

// The CCA calls requestCredis with the confidential pledge handle + a spend
// authorization that binds it to the user's bundle account. The CCA holds the
// `pledgeSecret` the user handed over (in the ticket for the demo); it does NOT
// hold the user's view key, so it cannot read the user's encrypted Gratis
// balance — only the pledge is consumed and the loan disbursed to the bundle.
//
// CLI: [ticketPath?] [envName?]
let ticketPath: string | undefined;
let envName = DEFAULT_ENV;
for (const a of process.argv.slice(2)) {
  if (a.endsWith(".json")) ticketPath = a;
  else envName = a;
}

const { envPath, deploymentEnvPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });
const envContext = `${envPath} or ${deploymentEnvPath}`;

const rpcUrl = requireEnv("RPC_URL", envContext);
const ccaPrivateKey = requireEnv("CCA_PRIVATE_KEY", envContext);
const ccaAddress = requireEnv("CCA_ADDRESS", envContext);
const userAddress = requireEnv("USER_ADDRESS", envContext);
const gratisAddress = process.env["GRATIS_ADDRESS"] || DEFAULT_GRATIS_ADDRESS;
const credisFactoryAddress = process.env["CREDIS_FACTORY_ADDRESS"] || DEFAULT_CREDIS_FACTORY_ADDRESS;
const credisAddress = process.env["CREDIS_ADDRESS"] || DEFAULT_CREDIS_ADDRESS;
const smartAccountFactoryAddress = requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envContext);
const vaultProviderAddress = requireEnv("VAULT_PROVIDER_ADDRESS", envContext);
const erc20Address = requireEnv("ERC20_ADDRESS", envContext);

function loadTicket(): { ticket: Ticket; path: string } {
  if (ticketPath) return { ticket: readTicket(ticketPath), path: ticketPath };
  const latest = findLatestTicket();
  if (!latest) {
    console.error("No ticket found under tickets/. Run `npm run pledge-gratis` first.");
    process.exit(1);
  }
  return latest;
}

async function main() {
  const { ticket, path: usedTicketPath } = loadTicket();

  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const ccaWallet = new Wallet(ccaPrivateKey, provider);

  // Predict the bundle account address — the credis receiver, and the account
  // the pledge spend is bound to.
  const saFactory = SmartAccountFactory__factory.connect(smartAccountFactoryAddress, provider);
  const bundleAccount = await saFactory.getAccountAddress(
    userAddress,
    ccaAddress,
    [erc20Address],
    [vaultProviderAddress],
    SALT,
  );

  const credisFactory = ICredisFactory__factory.connect(credisFactoryAddress, ccaWallet);
  const credis = ICredis__factory.connect(credisAddress, provider);
  const token = IERC20__factory.connect(erc20Address, provider);
  const gratis = IGratis__factory.connect(gratisAddress, provider);

  const [erc20Meta, network] = await Promise.all([fetchTokenMeta(token), provider.getNetwork()]);

  // Bind the pledge to this bundle account with the spend authorization derived
  // from the pledge secret the user handed to the CCA.
  const secret = ethers.getBytes(ticket.pledgeSecret);
  const spend = spendAuth(secret, bundleAccount);

  console.log("=== Request Credis (confidential / TEE) ===");
  console.log(`Env:            ${envName}`);
  console.log(`Ticket:         ${usedTicketPath}`);
  console.log(`CCA:            ${ccaAddress}`);
  console.log(`User (pledger): ${userAddress}`);
  console.log(`CredisFactory:  ${credisFactoryAddress}`);
  console.log(`Bundle account: ${bundleAccount}`);
  console.log(`ERC20:          ${erc20Address} (${erc20Meta.symbol})`);
  console.log(`Pledge handle:  ${ticket.pledgeHandle}`);
  console.log(`Spend auth:     ${spend}`);
  console.log(`Chain ID:       ${network.chainId}`);

  const bundleErc20Before = await token.balanceOf(bundleAccount);
  console.log(`\nBundle ERC20 before: ${formatTokenMeta(bundleErc20Before, erc20Meta)}`);

  // `userAddress` is the pledger EOA: the chain checks it against the pledge
  // record and debits its pledged ledger into the credis escrow.
  console.log("\nSending requestCredis(asset, bundleAccount, eoaAccount, pledgeHandle, spendAuth)...");
  const tx = await credisFactory.requestCredis(erc20Address, bundleAccount, userAddress, ticket.pledgeHandle, spend);
  console.log(`  TX hash: ${tx.hash}`);
  const receipt = await tx.wait();
  if (!receipt) throw new Error("requestCredis tx receipt missing");
  console.log(`  Block:   ${receipt.blockNumber}`);

  // Log the events across the involved interfaces.
  const interfaces = [
    { name: "ICredisFactory", iface: ICredisFactory__factory.createInterface() },
    { name: "ICredis", iface: ICredis__factory.createInterface() },
    { name: "VaultProvider", iface: IVaultProvider__factory.createInterface() },
    { name: "IGratis", iface: IGratis__factory.createInterface() },
    { name: "ERC20", iface: IERC20__factory.createInterface() },
  ];
  let eventPositionId: bigint | null = null;
  console.log("\n=== Transaction Events ===");
  for (const log of receipt.logs ?? []) {
    for (const { name, iface } of interfaces) {
      try {
        const event = iface.parseLog({ topics: log.topics as string[], data: log.data });
        if (event) {
          console.log(`  [${name}] ${event.name}`);
          if (event.name === "PositionCreated") eventPositionId = event.args[0] as bigint;
          break;
        }
      } catch {
        // not from this interface
      }
    }
  }

  // Position id is deterministic: keccak256(pledgeHandle || bundleAccount).
  const positionId = computePositionId(ticket.pledgeHandle, bundleAccount);
  if (eventPositionId !== null && eventPositionId !== positionId) {
    throw new Error(
      `PositionCreated id ${eventPositionId} != computed ${positionId} — check position_id parity`,
    );
  }

  const bundleErc20After = await token.balanceOf(bundleAccount);
  const position = await credis.getPosition(positionId);

  console.log("\n=== Position ===");
  console.log(`  positionId:        ${positionId}`);
  console.log(`  bundleAccount:     ${position.bundleAccount}`);
  console.log(`  credisPrincipal:   ${formatTokenMeta(position.credisPrincipal, erc20Meta)}`);
  console.log(`  totalAnadosis:     ${formatTokenMeta(position.totalAnadosisAmount, erc20Meta)}`);
  console.log(`  totalGratis:       ${position.totalGratisAmount}`);
  console.log(`  refinancingRate:   ${position.refinancingRate}`);
  console.log(`  issuanceCurrency:  ${position.issuanceCurrency}`);
  console.log(`\nBundle ERC20 change: ${formatTokenDiff(bundleErc20After - bundleErc20Before, erc20Meta.decimals, erc20Meta.symbol)}`);

  // Persist the position + bundle for pay-anadosis.
  ticket.positionId = positionId.toString();
  ticket.bundleAccount = bundleAccount;
  writeTicket(ticket);
  console.log(`\nTicket updated: ${usedTicketPath}`);
  console.log("Run `npm run user-pays-anadosis` to pay an installment (and unlock 1/N of the collateral).");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
