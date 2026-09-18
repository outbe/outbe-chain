import { ethers, Wallet } from "ethers";
import { IERC20__factory, IVaultRouter__factory, SmartAccountFactory__factory } from "./contracts/index.js";
import {
  DEFAULT_ENV,
  fetchTokenMeta,
  formatToken,
  loadEnv,
  requireEnv,
} from "./utils.js";
import { writeReservation } from "./ticket.js";

const SALT = 0n;

// CCA locks vault stables for the user's smart account before the user pledges.
// CLI: [amountStables] [envName]. Defaults to "1" unit of the stablecoin.
const amountArg = process.argv[2] || "1";
const envName = process.argv[3] || DEFAULT_ENV;

const { envPath, deploymentEnvPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });
const envContext = `${envPath} or ${deploymentEnvPath}`;

const rpcUrl = requireEnv("RPC_URL", envContext);
const ccaPrivateKey = requireEnv("CCA_PRIVATE_KEY", envContext);
const ccaAddress = requireEnv("CCA_ADDRESS", envContext);
const userAddress = requireEnv("USER_ADDRESS", envContext);
const smartAccountFactoryAddress = requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envContext);
const vaultRouterAddress = requireEnv("VAULT_ROUTER_ADDRESS", envContext);
const erc20Address = requireEnv("ERC20_ADDRESS", envContext);

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const ccaWallet = new Wallet(ccaPrivateKey, provider);
  const saFactory = SmartAccountFactory__factory.connect(smartAccountFactoryAddress, provider);
  const vaultRouter = IVaultRouter__factory.connect(vaultRouterAddress, ccaWallet);
  const token = IERC20__factory.connect(erc20Address, provider);
  const tokenMeta = await fetchTokenMeta(token);
  const amount = ethers.parseUnits(amountArg, tokenMeta.decimals);
  const { chainId } = await provider.getNetwork();

  const smartAccount = await saFactory.getAccountAddress(
    userAddress,
    ccaAddress,
    [erc20Address],
    [vaultRouterAddress],
    SALT,
  );

  console.log("=== Reserve stables (CCA) ===");
  console.log(`Env:           ${envName}`);
  console.log(`CCA:           ${ccaAddress}`);
  console.log(`smart account: ${smartAccount}`);
  console.log(`Asset:         ${erc20Address} (${tokenMeta.symbol})`);
  console.log(`Amount:        ${formatToken(amount, tokenMeta.decimals, tokenMeta.symbol)}`);

  const saCode = await provider.getCode(smartAccount);
  if (saCode === "0x") {
    console.error("smart account not deployed. Run `npm run top-up-sa` first.");
    process.exit(1);
  }

  console.log("\nSending reserveStables(smartAccount, asset, amount)...");
  const tx = await vaultRouter.reserveStables(smartAccount, erc20Address, amount);
  const receipt = await tx.wait();
  if (!receipt) throw new Error("reserveStables tx receipt missing");

  const created = receipt.logs
    .map((log) => {
      try {
        return vaultRouter.interface.parseLog({ topics: log.topics as string[], data: log.data });
      } catch {
        return null;
      }
    })
    .find((event) => event?.name === "ReservationCreated");
  const rawId = created?.args?.id ?? created?.args?.[0];
  if (rawId === undefined || rawId === null) throw new Error("ReservationCreated event missing");
  const reservationId = rawId.toString();

  const path = writeReservation({
    reservationId,
    smartAccount,
    asset: erc20Address,
    amount: amount.toString(),
    blockNumber: receipt.blockNumber,
    txHash: receipt.hash,
    chainId: chainId.toString(),
    createdAt: new Date().toISOString(),
  });

  console.log(`  TX hash:      ${tx.hash}`);
  console.log(`  reservation:  ${reservationId}`);
  console.log(`  written:      ${path}`);
  console.log("\nUser can now `npm run pledge-gratis` against this hold.");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
