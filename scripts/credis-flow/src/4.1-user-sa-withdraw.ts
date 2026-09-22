import { ethers, Wallet } from "ethers";
import { SmartAccountFactory__factory, IERC20__factory } from "./contracts/index.js";
import { DEFAULT_ENV, loadEnv, requireEnv, fetchTokenMeta } from "./utils.js";
import { delayedOwnerExecution, singleExecution } from "./owner-execution.js";

async function main() {
  const amount = process.argv[2];
  if (!amount) throw new Error("Usage: npm run user-sa-withdraw -- <amount> [envName]; rerun to resume after five minutes.");
  const { envPath } = loadEnv(import.meta.url, process.argv[3] || DEFAULT_ENV, { deploymentEnv: true });
  const env = (key: string) => requireEnv(key, envPath);
  const provider = new ethers.JsonRpcProvider(env("RPC_URL"));
  const wallet = new Wallet(env("USER_PRIVATE_KEY"), provider);
  const factory = SmartAccountFactory__factory.connect(env("SMART_ACCOUNT_FACTORY_ADDRESS"), provider);
  const token = IERC20__factory.connect(env("ERC20_ADDRESS"), provider);
  const account = await factory.getAccountAddress(wallet.address, env("CCA_ADDRESS"), [await token.getAddress()], 0n);
  const value = ethers.parseUnits(amount, (await fetchTokenMeta(token)).decimals);
  if (value <= 0n || await token.balanceOf(account) < value) throw new Error("Invalid amount or insufficient account balance");
  const execution = singleExecution(await token.getAddress(), token.interface.encodeFunctionData("transfer", [wallet.address, value]));
  const receipt = await delayedOwnerExecution(wallet, account, env("ENTRYPOINT_ADDRESS"), await factory.executionDelayPolicy(), `withdraw-${value}`, execution);
  if (receipt) console.log(`Withdrawal completed: ${receipt.hash}`);
}
main().catch(error => { console.error(error); process.exitCode = 1; });
