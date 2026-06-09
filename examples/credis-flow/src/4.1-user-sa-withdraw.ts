import { ethers, Wallet } from "ethers";
import {
  SmartAccountFactory__factory,
  IERC20__factory,
  ITokenBundle__factory,
  IEntryPoint__factory,
} from "./contracts/index.js";
import {
  formatTokenMeta,
  fetchTokenMeta,
  TokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv, formatTokenMeta2,
} from "./utils.js";

const SALT = 0n;

// Parse CLI args: <amount> [envName]
if (!process.argv[2]) {
  console.error("Usage: npx tsx src/4.1-user-sa-withdraw.ts <amount> [envName]");
  console.error("  amount  - withdrawal amount in human-readable format (e.g. 5.5 for 5.5 tokens)");
  console.error("  envName - environment name (default: local-dev)");
  process.exit(1);
}

const withdrawAmountArg = process.argv[2];
const envName = process.argv[3] || DEFAULT_ENV;

// Load env files
const { envPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });

const rpcUrl = requireEnv("RPC_URL", envPath);
const userPrivateKey = requireEnv("USER_PRIVATE_KEY", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const ccaAddress = requireEnv("CCA_ADDRESS", envPath);
const smartAccountFactoryAddress = requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envPath);
const bundleModulePluginAddress = requireEnv("BUNDLE_MODULE_PLUGIN_ADDRESS", envPath);
const entryPointAddress = requireEnv("ENTRYPOINT_ADDRESS", envPath);
const ecdsaValidatorAddress = requireEnv("ECDSA_VALIDATOR_ADDRESS", envPath);
const erc20Address = requireEnv("ERC20_ADDRESS", envPath);
const vaultProviderAddress = requireEnv("VAULT_PROVIDER_ADDRESS", envPath);

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const userWallet = new Wallet(userPrivateKey, provider);

  const saFactory = SmartAccountFactory__factory.connect(smartAccountFactoryAddress, provider);
  const token = IERC20__factory.connect(erc20Address, provider);
  const bundlePlugin = ITokenBundle__factory.connect(bundleModulePluginAddress, provider);

  const erc20Meta = await fetchTokenMeta(token);
  const WITHDRAW_AMOUNT = ethers.parseUnits(withdrawAmountArg, erc20Meta.decimals);

  // Predict Bundle account address
  const smartAccountAddr = await saFactory.getAccountAddress(
    userAddress,
    ccaAddress,
    [erc20Address],
    [vaultProviderAddress],
    SALT,
  );

  console.log("=== User Bundle Account Withdraw ===");
  console.log(`Env:              ${envName}`);
  console.log(`RPC:              ${rpcUrl}`);
  console.log(`User:             ${userAddress}`);
  console.log(`Bundle Account:    ${smartAccountAddr}`);
  console.log(`EntryPoint:       ${entryPointAddress}`);
  console.log(`ECDSA Validator:  ${ecdsaValidatorAddress}`);
  console.log(`ERC20:            ${erc20Address} (${erc20Meta.symbol})`);
  console.log(`Withdraw amount:  ${formatTokenMeta(WITHDRAW_AMOUNT, erc20Meta)}`);

  // Verify smart account is deployed
  const code = await provider.getCode(smartAccountAddr);
  if (code === "0x") {
    console.error("Bundle account not deployed. Run 2-top-up-smart-account.ts first.");
    process.exit(1);
  }

  // State before
  const [bundleBalBefore, accountBalBefore, userBalBefore] = await Promise.all([
    bundlePlugin.balanceOf(smartAccountAddr, erc20Address).catch(() => 0n),
    token.balanceOf(smartAccountAddr),
    token.balanceOf(userAddress),
  ]);

  console.log("\n=== State BEFORE ===");
  printBalances(accountBalBefore, bundleBalBefore, userBalBefore, smartAccountAddr, erc20Meta);

  const personalBal = accountBalBefore - bundleBalBefore;
  if (personalBal < WITHDRAW_AMOUNT) {
    console.error(`Insufficient personal balance: have ${formatTokenMeta(personalBal, erc20Meta)}, need ${formatTokenMeta(WITHDRAW_AMOUNT, erc20Meta)}`);
    process.exit(1);
  }

  // ── Build UserOp with root validation (user/owner) ────────────────────────

  // nonceKey = mode(0x00) | vType(0x00=ROOT) | ecdsaValidatorAddress(20 bytes) | parallelKey(0x0000)
  const validatorHex = ecdsaValidatorAddress.slice(2).toLowerCase(); // 20 bytes without 0x
  const nonceKeyHex = "0000" + validatorHex + "0000"; // 24 bytes
  const nonceKey = BigInt("0x" + nonceKeyHex);

  const entryPoint = IEntryPoint__factory.connect(entryPointAddress, userWallet);

  const nonce = await entryPoint.getNonce(smartAccountAddr, nonceKey);

  // Ensure EntryPoint has deposit for gas
  const epDeposit: bigint = await entryPoint.balanceOf(smartAccountAddr);
  if (epDeposit < ethers.parseEther("0.01")) {
    console.log("\nFunding EntryPoint deposit for Bundle account...");
    const depositTx = await entryPoint.depositTo(smartAccountAddr, { value: ethers.parseEther("0.05") });
    await depositTx.wait();
    console.log("  Deposited 0.05 COEN into EntryPoint");
  }

  // callData = executeUserOp.selector || execute(execMode, encodeSingle(token, 0, transfer(user, amount)))
  const erc20Iface = new ethers.Interface(["function transfer(address to, uint256 amount) returns (bool)"]);
  const transferCalldata = erc20Iface.encodeFunctionData("transfer", [userAddress, WITHDRAW_AMOUNT]);
  const executionCalldata = ethers.solidityPacked(
    ["address", "uint256", "bytes"],
    [erc20Address, 0n, transferCalldata],
  );
  const execModeBytes32 = "0x" + "00".repeat(32);
  const kernelIface = new ethers.Interface([
    "function execute(bytes32 mode, bytes calldata executionCalldata)",
  ]);
  const innerExecute = kernelIface.encodeFunctionData("execute", [execModeBytes32, executionCalldata]);
  const executeUserOpSel = "0x8dd7712f";
  const callData = ethers.concat([executeUserOpSel, innerExecute]);

  const accountGasLimits = ethers.solidityPacked(["uint128", "uint128"], [2_000_000n, 2_000_000n]);
  const gasFees = ethers.solidityPacked(["uint128", "uint128"], [1n, 1n]);

  const op = {
    sender: smartAccountAddr,
    nonce: nonce,
    initCode: "0x",
    callData: callData,
    accountGasLimits: accountGasLimits,
    preVerificationGas: 1_000_000n,
    gasFees: gasFees,
    paymasterAndData: "0x",
    signature: "0x",
  };

  // Sign with user key — root validation uses raw ECDSA signature (no 0xFF prefix)
  const userOpHash = await entryPoint.getUserOpHash(op);
  const sig = await userWallet.signMessage(ethers.getBytes(userOpHash));
  op.signature = sig;

  console.log("\nSending UserOp via EntryPoint.handleOps...");
  console.log(`  Nonce:      ${nonce}`);
  console.log(`  UserOpHash: ${userOpHash}`);

  const tx = await entryPoint.handleOps([op], userWallet.address);
  const receipt = await tx.wait();
  console.log(`  TX hash:    ${receipt!.hash}`);
  console.log(`  Block:      ${receipt!.blockNumber}`);
  console.log(`  Gas used:   ${receipt!.gasUsed}`);

  // ── State after ───────────────────────────────────────────────────────────

  const [bundleBalAfter, accountBalAfter, userBalAfter] = await Promise.all([
    bundlePlugin.balanceOf(smartAccountAddr, erc20Address).catch(() => 0n),
    token.balanceOf(smartAccountAddr),
    token.balanceOf(userAddress),
  ]);

  console.log("\n=== State AFTER ===");
  printBalances(accountBalAfter, bundleBalAfter, userBalAfter, smartAccountAddr, erc20Meta);

  console.log("\n=== CHANGES ===");
  const bundleDiff = bundleBalAfter - bundleBalBefore;
  const accountDiff = accountBalAfter - accountBalBefore;
  const userDiff = userBalAfter - userBalBefore;
  console.log(`  SA total:     ${accountDiff >= 0n ? "+" : ""}${formatTokenMeta(accountDiff, erc20Meta)}`);
  console.log(`  SA bundle:    ${bundleDiff >= 0n ? "+" : ""}${formatTokenMeta(bundleDiff, erc20Meta)}`);
  console.log(`  User EOA:     ${userDiff >= 0n ? "+" : ""}${formatTokenMeta(userDiff, erc20Meta)}`);
}

function printBalances(
  accountBal: bigint,
  bundleBal: bigint,
  userBal: bigint,
  smartAccountAddr: string,
  erc20Meta: TokenMeta,
) {
  const personalBal = accountBal - bundleBal;
  const bundleBalance2 = bundleBal / 2n;
  console.log(`  Bundle Account (${smartAccountAddr}):`);
  console.log(`    ERC20 total:   ${formatTokenMeta(accountBal, erc20Meta)}`);
  console.log(`    Bundle:        ${formatTokenMeta(bundleBal, erc20Meta)} (${formatTokenMeta2(bundleBalance2, erc20Meta)} + ${formatTokenMeta2(bundleBalance2, erc20Meta)})`);
  console.log(`    Personal:      ${formatTokenMeta(personalBal, erc20Meta)}`);
  console.log(`  User EOA (${userAddress}):`);
  console.log(`    ERC20 balance: ${formatTokenMeta(userBal, erc20Meta)}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
