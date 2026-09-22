import { ethers, Wallet } from "ethers";
import {
  SmartAccountFactory__factory,
  IERC20__factory,
  IEntryPoint__factory,
} from "./contracts/index.js";
import {
  formatTokenMeta,
  fetchTokenMeta,
  TokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv, formatTokenMeta2,
  ccaPermissionId,
  permissionNonceKey,
  encodePermissionSignature,
  ENTRYPOINT_MIN_DEPOSIT,
  ENTRYPOINT_TOPUP,
  formatCoen,
} from "./utils.js";

const SALT = 0n;
const WITHDRAW_AMOUNT = 1_000_000n; // 1 USDT0 (6 decimals)

// Parse CLI args: [envName]
const envName = process.argv[2] || DEFAULT_ENV;

// Load env files
const { envPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });

const rpcUrl = requireEnv("RPC_URL", envPath);
const userAddress = requireEnv("USER_ADDRESS", envPath);
const ccaPrivateKey = requireEnv("CCA_PRIVATE_KEY", envPath);
const ccaAddress = requireEnv("CCA_ADDRESS", envPath);
const smartAccountFactoryAddress = requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envPath);
const entryPointAddress = requireEnv("ENTRYPOINT_ADDRESS", envPath);
const erc20Address = requireEnv("ERC20_ADDRESS", envPath);
const vaultRouterAddress = requireEnv("VAULT_ROUTER_ADDRESS", envPath);

async function main() {
  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const ccaWallet = new Wallet(ccaPrivateKey, provider);

  const saFactory = SmartAccountFactory__factory.connect(smartAccountFactoryAddress, provider);
  const token = IERC20__factory.connect(erc20Address, provider);

  const erc20Meta = await fetchTokenMeta(token);

  // Predict smart account address
  const smartAccountAddr = await saFactory.getAccountAddress(
    userAddress,
    ccaAddress,
    [erc20Address],
    SALT,
  );

  console.log("=== CCA Simulate Purchase ===");
  console.log(`Env:              ${envName}`);
  console.log(`RPC:              ${rpcUrl}`);
  console.log(`User:             ${userAddress}`);
  console.log(`CCA:              ${ccaAddress}`);
  console.log(`smart account:    ${smartAccountAddr}`);
  console.log(`EntryPoint:       ${entryPointAddress}`);
  console.log(`ERC20:            ${erc20Address} (${erc20Meta.symbol})`);
  console.log(`Withdraw amount:  ${formatTokenMeta(WITHDRAW_AMOUNT, erc20Meta)}`);

  // Verify smart account is deployed
  const code = await provider.getCode(smartAccountAddr);
  if (code === "0x") {
    console.error("smart account not deployed. Run `npm run setup-account` first.");
    process.exit(1);
  }

  // State before
  const [accountBalBefore, ccaBalBefore] = await Promise.all([
    token.balanceOf(smartAccountAddr),
    token.balanceOf(ccaAddress),
  ]);

  console.log("\n=== State BEFORE ===");
  printBalances(smartAccountAddr, accountBalBefore, ccaBalBefore, erc20Meta);

  if (accountBalBefore < WITHDRAW_AMOUNT) {
    console.error(`Insufficient account balance: have ${formatTokenMeta(accountBalBefore, erc20Meta)}, need ${formatTokenMeta(WITHDRAW_AMOUNT, erc20Meta)}`);
    process.exit(1);
  }

  // -- Build and submit UserOp -----------------------------------------------

  // Per-token CCA permission (Kernel v4 permission validation, vType = 0x02).
  const nonceKey = permissionNonceKey(ccaPermissionId(erc20Address));

  const entryPoint = IEntryPoint__factory.connect(entryPointAddress, ccaWallet);

  const nonce = await entryPoint.getNonce(smartAccountAddr, nonceKey);

  // Ensure EntryPoint has deposit for gas
  const epDeposit: bigint = await entryPoint.balanceOf(smartAccountAddr);
  if (epDeposit < ENTRYPOINT_MIN_DEPOSIT) {
    console.log("\nFunding EntryPoint deposit for smart account...");
    const depositTx = await entryPoint.depositTo(smartAccountAddr, { value: ENTRYPOINT_TOPUP });
    await depositTx.wait();
    console.log(`  Deposited ${formatCoen(ENTRYPOINT_TOPUP)} COEN into EntryPoint`);
  }

  // CCA has no execution hook: call execute directly under its restrictive policy.
  const erc20Iface = new ethers.Interface(["function transfer(address to, uint256 amount) returns (bool)"]);
  const transferCalldata = erc20Iface.encodeFunctionData("transfer", [ccaAddress, WITHDRAW_AMOUNT]);
  const executionCalldata = ethers.solidityPacked(
    ["address", "uint256", "bytes"],
    [erc20Address, 0n, transferCalldata],
  );
  const execModeBytes32 = "0x" + "00".repeat(32);
  const kernelIface = new ethers.Interface([
    "function execute(bytes32 mode, bytes calldata executionCalldata)",
  ]);
  const innerExecute = kernelIface.encodeFunctionData("execute", [execModeBytes32, executionCalldata]);
  const callData = innerExecute;

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

  // Kernel v4 permission signature: abi.encode(bytes[]{ policy slice (empty), CCA ECDSA sig }).
  const userOpHash = await entryPoint.getUserOpHash(op);
  const sig = await ccaWallet.signMessage(ethers.getBytes(userOpHash));
  op.signature = encodePermissionSignature(sig);

  console.log("\nSending UserOp via EntryPoint.handleOps...");
  console.log(`  Nonce:      ${nonce}`);
  console.log(`  UserOpHash: ${userOpHash}`);

  const tx = await entryPoint.handleOps([op], ccaWallet.address);
  const receipt = await tx.wait();
  const result = receipt?.logs.filter(log => log.address.toLowerCase() === entryPointAddress.toLowerCase())
    .map(log => { try { return entryPoint.interface.parseLog(log); } catch { return null; } })
    .find(log => log?.name === "UserOperationEvent" && log.args.userOpHash === userOpHash);
  if (!result?.args.success) throw new Error("CCA withdrawal execution failed");
  console.log(`  TX hash:    ${receipt!.hash}`);
  console.log(`  Block:      ${receipt!.blockNumber}`);
  console.log(`  Gas used:   ${receipt!.gasUsed}`);

  // -- State after -----------------------------------------------------------

  const [accountBalAfter, ccaBalAfter] = await Promise.all([
    token.balanceOf(smartAccountAddr),
    token.balanceOf(ccaAddress),
  ]);

  console.log("\n=== State AFTER ===");
  printBalances(smartAccountAddr, accountBalAfter, ccaBalAfter, erc20Meta);

  console.log("\n=== CHANGES ===");
  const accountDiff = accountBalAfter - accountBalBefore;
  const ccaDiff = ccaBalAfter - ccaBalBefore;
  console.log(`  SA total:     ${accountDiff >= 0n ? "+" : ""}${formatTokenMeta(accountDiff, erc20Meta)}`);
  console.log(`  CCA ERC20:    ${ccaDiff >= 0n ? "+" : ""}${formatTokenMeta(ccaDiff, erc20Meta)}`);
}

function printBalances(
  smartAccountAddr: string,
  accountBal: bigint,
  ccaBal: bigint,
  erc20Meta: TokenMeta,
) {
  console.log(`  smart account (${smartAccountAddr}):`);
  console.log(`    ERC20 total:   ${formatTokenMeta(accountBal, erc20Meta)}`);
  console.log(`  CCA (${ccaAddress}):`);
  console.log(`    ERC20 balance: ${formatTokenMeta(ccaBal, erc20Meta)}`);
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
