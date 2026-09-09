import { ethers, Wallet } from "ethers";
import { SmartAccountFactory__factory, ITokenBundle__factory } from "./contracts/index.js";
import { DEFAULT_ENV, loadEnv, requireEnv } from "./utils.js";
import { ownerCall } from "./smart-account.js";

async function main() {
  const { envPath } = loadEnv(import.meta.url, process.argv[2] || DEFAULT_ENV, { deploymentEnv: true });
  const provider = new ethers.JsonRpcProvider(requireEnv("RPC_URL", envPath));
  const wallet = new Wallet(requireEnv("USER_PRIVATE_KEY", envPath), provider);
  const factory = SmartAccountFactory__factory.connect(requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envPath), wallet);
  const account = await factory.getAccountAddress(wallet.address, 0n);
  if (await provider.getCode(account) === "0x") throw new Error("Account is not deployed");
  const custody = ITokenBundle__factory.connect(await factory.bundleModulePlugin(), provider);
  if (await custody.status(account) === 2n) { console.log("Bundle already permanently closed"); return; }
  for (const token of await custody.bundleTokensOf(account)) {
    if (await custody.balanceOf(account, token) !== 0n) throw new Error(`Bundle has a non-zero reserve: ${token}`);
  }
  await ownerCall(wallet, requireEnv("ENTRYPOINT_ADDRESS", envPath), account, account,
    new ethers.Interface(["function closeBundle()"]).encodeFunctionData("closeBundle"));
  console.log(`Bundle permanently closed: ${account}`);
}
main().catch(error => { console.error(error); process.exitCode = 1; });
