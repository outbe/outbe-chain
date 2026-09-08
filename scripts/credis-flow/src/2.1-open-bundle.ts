import { ethers, Wallet } from "ethers";
import { SmartAccountFactory__factory, ITokenBundle__factory, IERC20__factory } from "./contracts/index.js";
import { DEFAULT_ENV, loadEnv, requireEnv, encodePermissionSignature } from "./utils.js";
import { ownerCall } from "./smart-account.js";

async function main() {
  const { envPath } = loadEnv(import.meta.url, process.argv[2] || DEFAULT_ENV, { deploymentEnv: true });
  const provider = new ethers.JsonRpcProvider(requireEnv("RPC_URL", envPath));
  const wallet = new Wallet(requireEnv("USER_PRIVATE_KEY", envPath), provider);
  const factory = SmartAccountFactory__factory.connect(requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envPath), wallet);
  const account = await factory.getAccountAddress(wallet.address, 0n);
  if (await provider.getCode(account) === "0x") throw new Error("Create the account with npm run top-up-sa first");
  const custody = ITokenBundle__factory.connect(await factory.bundleModulePlugin(), wallet);
  const cca = requireEnv("CCA_ADDRESS", envPath);
  const tokens = [requireEnv("ERC20_ADDRESS", envPath)];
  const senders = [requireEnv("VAULT_ROUTER_ADDRESS", envPath)];
  const state = await custody.status(account);
  if (state === 2n) throw new Error("Bundle eligibility is permanently closed");
  if (state === 0n) {
    const packages = (await factory.getBundleInstallPackages(cca, tokens, senders)).map(p => ({
      moduleType: p.moduleType, module: p.module, moduleData: p.moduleData, internalData: p.internalData,
    }));
    const kernel = new ethers.Contract(account, ["function nonce(uint192) view returns(uint256)"], provider);
    const nonce: bigint = await kernel.getFunction("nonce")(0n);
    const { chainId } = await provider.getNetwork();
    const signature = await wallet.signTypedData({ name: "Kernel", version: "0.4.0", chainId, verifyingContract: account }, {
      Install: [{ name: "moduleType", type: "uint256" }, { name: "module", type: "address" },
        { name: "moduleData", type: "bytes" }, { name: "internalData", type: "bytes" }],
      InstallPackages: [{ name: "nonce", type: "uint256" }, { name: "packages", type: "Install[]" }],
    }, { nonce, packages });
    await (await factory.openBundle(account, cca, tokens, senders, nonce, encodePermissionSignature(signature))).wait();
  } else if ((await custody.linkedCca(account)).toLowerCase() !== cca.toLowerCase() ||
      (await custody.bundleTokensOf(account)).map(x => x.toLowerCase()).join() !== tokens.map(x => x.toLowerCase()).join() ||
      (await custody.bundleSendersOf(account)).map(x => x.toLowerCase()).join() !== senders.map(x => x.toLowerCase()).join()) {
    throw new Error("Existing bundle configuration differs from this environment");
  }
  // Authorize only the matching contribution specified by the user (default: 1,000 six-decimal tokens).
  const matchingAmount = ethers.parseUnits(process.argv[3] || "1000", 6);
  const token = IERC20__factory.connect(tokens[0], wallet);
  await ownerCall(wallet, requireEnv("ENTRYPOINT_ADDRESS", envPath), account, tokens[0],
    token.interface.encodeFunctionData("approve", [await custody.getAddress(), matchingAmount]));
  console.log(`Bundle open: ${account}; matching contribution approved: ${matchingAmount}`);
}
main().catch(error => { console.error(error); process.exitCode = 1; });
