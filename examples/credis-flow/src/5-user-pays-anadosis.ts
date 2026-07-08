import { ethers, Wallet } from "ethers";
import {
  ICredisFactory__factory,
  ICredis__factory,
  SmartAccountFactory__factory,
  IERC20__factory,
  IVaultProvider__factory,
  IGratis__factory,
  IGratisPool__factory,
  IEntryPoint__factory,
} from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_CREDIS_FACTORY_ADDRESS,
  DEFAULT_CREDIS_ADDRESS,
  formatTokenMeta,
  formatTokenDiff,
  fetchTokenMeta,
  TokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv, formatToken,
  anadosisDenomByAmount,
} from "./utils.js";
import { commitmentHash, fieldToHex32, toField } from "./shielded.js";
import { writeTicket } from "./ticket.js";

const SALT = 0n;

// Parse CLI args: <positionId> [envName]
if (!process.argv[2]) {
  console.error("Usage: npx tsx src/5-user-pays-anadosis.ts <positionId> [envName]");
  console.error("  positionId  - credis position ID");
  console.error("  envName     - environment name (default: local-dev)");
  process.exit(1);
}

const positionId = BigInt(process.argv[2]);
const envName = process.argv[3] || DEFAULT_ENV;

// Load env files
const { envPath, deploymentEnvPath } = loadEnv(import.meta.url, envName, { deploymentEnv: true });
const envContext = `${envPath} or ${deploymentEnvPath}`;

const rpcUrl = requireEnv("RPC_URL", envContext);
const userPrivateKey = requireEnv("USER_PRIVATE_KEY", envContext);
const userAddress = requireEnv("USER_ADDRESS", envContext);
const ccaAddress = requireEnv("CCA_ADDRESS", envContext);
const credisFactoryAddress = process.env["CREDIS_FACTORY_ADDRESS"] || DEFAULT_CREDIS_FACTORY_ADDRESS;
const credisAddress = process.env["CREDIS_ADDRESS"] || DEFAULT_CREDIS_ADDRESS;
const smartAccountFactoryAddress = requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envContext);
const ecdsaValidatorAddress = requireEnv("ECDSA_VALIDATOR_ADDRESS", envContext);
const entryPointAddress = requireEnv("ENTRYPOINT_ADDRESS", envContext);
const erc20Address = requireEnv("ERC20_ADDRESS", envContext);
const vaultProviderAddress = requireEnv("VAULT_PROVIDER_ADDRESS", envContext);

function formatDate(timestamp: bigint): string {
  if (timestamp === 0n) return "N/A";
  return new Date(Number(timestamp) * 1000).toISOString();
}

const ERROR_STRING_SELECTOR = "0x08c379a0";
const PANIC_SELECTOR = "0x4e487b71";
const KNOWN_ERROR_SIGS: Record<string, string> = {
  // Kernel.executeUserOp wraps inner failure in this error
  [ethers.id("ExecutionReverted()").slice(0, 10)]: "ExecutionReverted()",
  [ethers.id("InsufficientFreeBalance()").slice(0, 10)]: "InsufficientFreeBalance()",
  [ethers.id("InvalidCallType()").slice(0, 10)]: "InvalidCallType()",
  [ethers.id("InvalidSelector()").slice(0, 10)]: "InvalidSelector()",
};

function decodeRevert(data: string): string {
  if (!data || data === "0x") return "(empty)";
  const sel = data.slice(0, 10).toLowerCase();
  const abi = ethers.AbiCoder.defaultAbiCoder();
  try {
    if (sel === ERROR_STRING_SELECTOR) {
      const [reason] = abi.decode(["string"], "0x" + data.slice(10));
      return `Error("${reason}")`;
    }
    if (sel === PANIC_SELECTOR) {
      const [code] = abi.decode(["uint256"], "0x" + data.slice(10));
      return `Panic(0x${code.toString(16)})`;
    }
  } catch {
    // fall through to raw
  }
  if (KNOWN_ERROR_SIGS[sel]) return KNOWN_ERROR_SIGS[sel];
  const bytes = data.startsWith("0x") ? data.slice(2) : data;
  if (bytes.length > 0 && bytes.length % 2 === 0) {
    const buf = Buffer.from(bytes, "hex");
    if (buf.every((b) => (b >= 0x20 && b < 0x7f) || b === 0x0a || b === 0x09)) {
      return `"${buf.toString("utf8")}"`;
    }
  }
  return `raw=${data}`;
}

interface State {
  saErc20Balance: bigint;
  vaultErc20Balance: bigint;
  hasOverdue: boolean;
}

async function getState(
  token: ReturnType<typeof IERC20__factory.connect>,
  credis: ReturnType<typeof ICredis__factory.connect>,
  smartAccountAddr: string,
  underlyingVaultAddr: string,
): Promise<State> {
  const [saErc20Balance, vaultErc20Balance, hasOverdue] = await Promise.all([
    token.balanceOf(smartAccountAddr),
    token.balanceOf(underlyingVaultAddr),
    credis.hasOverdueAnadosis(smartAccountAddr).catch(() => false),
  ]);
  return { saErc20Balance, vaultErc20Balance, hasOverdue };
}

function printState(label: string, state: State, erc20Meta: TokenMeta, smartAccountAddr: string) {
  console.log(`\n=== ${label} ===`);
  console.log(`  Bundle Account (${smartAccountAddr}):`);
  console.log(`    ERC20 balance: ${formatTokenMeta(state.saErc20Balance, erc20Meta)}`);
  console.log(`  Vault Provider (${vaultProviderAddress}):`);
  console.log(`    Vault ERC20:   ${formatTokenMeta(state.vaultErc20Balance, erc20Meta)}`);
  console.log(`  Credis:`);
  console.log(`    Has overdue:   ${state.hasOverdue}`);
}

async function main() {
  console.log("=== User Pays Anadosis ===");
  console.log(`Env:              ${envName}`);
  console.log(`RPC:              ${rpcUrl}`);
  console.log(`User:             ${userAddress}`);
  console.log(`CredisFactory:    ${credisFactoryAddress}`);
  console.log(`Credis:           ${credisAddress}`);
  console.log(`ERC20:            ${erc20Address}`);
  console.log(`Vault Provider:   ${vaultProviderAddress}`);
  console.log(`Position ID:      ${positionId}`);

  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const userWallet = new Wallet(userPrivateKey, provider);

  const saFactory = SmartAccountFactory__factory.connect(smartAccountFactoryAddress, provider);
  const token = IERC20__factory.connect(erc20Address, provider);
  const credis = ICredis__factory.connect(credisAddress, provider);

  const vaultProvider = IVaultProvider__factory.connect(vaultProviderAddress, provider);

  const [erc20Meta, underlyingVaultAddr] = await Promise.all([
    fetchTokenMeta(token),
    vaultProvider.assetVaultAt(erc20Address, 0),
  ]);

  // Predict Bundle account address
  const smartAccountAddr = await saFactory.getAccountAddress(
    userAddress,
    ccaAddress,
    [erc20Address],
    [vaultProviderAddress],
    SALT,
  );
  console.log(`Bundle Account:    ${smartAccountAddr}`);

  // Verify Bundle account is deployed
  const code = await provider.getCode(smartAccountAddr);
  if (code === "0x") {
    console.error("Bundle account not deployed. Run 2-top-up-smart-account.ts first.");
    process.exit(1);
  }

  // Fetch position to validate it exists
  const position = await credis.getPosition(positionId);
  if (position.createdAt === 0n) {
    console.error(`Position ${positionId} does not exist.`);
    process.exit(1);
  }
  console.log(`\nPosition:`);
  console.log(`  Bundle Account: ${position.bundleAccount}`);
  console.log(`  Total:         ${formatTokenMeta(position.totalAnadosisAmount, erc20Meta)}`);
  console.log(`  Outstanding:   ${formatTokenMeta(position.outstandingAnadosisAmount, erc20Meta)}`);
  console.log(`  Created:       ${formatDate(position.createdAt)}`);

  if (position.outstandingAnadosisAmount === 0n) {
    console.error("Position is fully paid. No anadosis remaining.");
    process.exit(1);
  }

  // Get next anadosis to determine approve amount
  const nextAnadosis = await credis.getNextAnadosis(positionId);
  const anadosisAmount: bigint = nextAnadosis.anadosisAmount;
  console.log(`\nNext anadosis #${nextAnadosis.anadosisNumber}:`);
  console.log(`  Due:           ${formatDate(nextAnadosis.dueDate)}`);
  console.log(`  Amount:        ${formatTokenMeta(anadosisAmount, erc20Meta)}`);
  console.log(`  Gratis amount: ${formatToken(nextAnadosis.gratisAmount, 18, "GRATIS")}`);

  if (anadosisAmount === 0n) {
    console.error("Next anadosis amount is 0. Nothing to pay.");
    process.exit(1);
  }

  // Derive the anadosis (one-decade-down) denomination for this installment's
  // reclaim note. Its amount == nextAnadosis.gratisAmount == pledge / 10. The
  // reclaim commitment MUST be computed with this denom id — the chain stores
  // it opaquely and cannot detect a wrong-denom note (it would be unspendable).
  const anadosisDenom = anadosisDenomByAmount(nextAnadosis.gratisAmount);
  const reclaimSecret = toField(ethers.getBytes(ethers.hexlify(ethers.randomBytes(32))));
  const reclaimNullifierSecret = toField(ethers.getBytes(ethers.hexlify(ethers.randomBytes(32))));
  const reclaimCommitment = await commitmentHash(reclaimSecret, reclaimNullifierSecret, anadosisDenom.id);
  console.log(`\nReclaim note for this installment:`);
  console.log(`  Anadosis denom:   ${anadosisDenom.id} = ${formatToken(anadosisDenom.amount, 18, "GRATIS")}`);
  console.log(`  Reclaim secret:   ${fieldToHex32(reclaimSecret)}`);
  console.log(`  Reclaim nullSecr: ${fieldToHex32(reclaimNullifierSecret)}`);
  console.log(`  Reclaim commit:   ${fieldToHex32(reclaimCommitment)}`);

  // State before
  const before = await getState(token, credis, smartAccountAddr, underlyingVaultAddr);
  printState("State BEFORE", before, erc20Meta, smartAccountAddr);

  if (before.saErc20Balance < anadosisAmount) {
    console.error(`Insufficient SA balance: have ${formatTokenMeta(before.saErc20Balance, erc20Meta)}, need ${formatTokenMeta(anadosisAmount, erc20Meta)}`);
    process.exit(1);
  }

  // ── Build batch UserOp: approve + anadosis ────────────────────────────

  // nonceKey = mode(0x00) | vType(0x00=ROOT) | ecdsaValidatorAddress(20 bytes) | parallelKey(0x0000)
  const validatorHex = ecdsaValidatorAddress.slice(2).toLowerCase();
  const nonceKeyHex = "0000" + validatorHex + "0000";
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

  // Encode batch: [approve(credisFactory, anadosisAmount), anadosis(positionId, reclaimCommitment)]
  // anadosis now carries this installment's reclaim commitment; the runtime
  // inserts it into the anadosis-denom pool so the note can be unpledged for
  // one tenth of the pledge immediately after this payment.
  const approveCalldata = IERC20__factory.createInterface().encodeFunctionData("approve", [credisFactoryAddress, anadosisAmount]);
  const payCalldata = ICredisFactory__factory.createInterface().encodeFunctionData("anadosis", [positionId, reclaimCommitment]);

  // Batch execution: execMode byte[0] = 0x01 (CALLTYPE_BATCH)
  const execModeBatch = "0x01" + "00".repeat(31);
  const abiCoder = ethers.AbiCoder.defaultAbiCoder();
  const executionCalldata = abiCoder.encode(
    ["tuple(address,uint256,bytes)[]"],
    [[
      [erc20Address, 0n, approveCalldata],
      [credisFactoryAddress, 0n, payCalldata],
    ]],
  );

  const kernelIface = new ethers.Interface([
    "function execute(bytes32 mode, bytes calldata executionCalldata)",
  ]);
  const innerExecute = kernelIface.encodeFunctionData("execute", [execModeBatch, executionCalldata]);
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

  // Sign with user key — root validation
  const userOpHash = await entryPoint.getUserOpHash(op);
  const sig = await userWallet.signMessage(ethers.getBytes(userOpHash));
  op.signature = sig;

  // ── Pre-simulate the inner execute() so a precompile revert surfaces here
  // rather than being swallowed by handleOps (which catches inner reverts and
  // still reports the outer tx as successful).
  console.log("\nSimulating inner execute() from EntryPoint...");
  try {
    await provider.call({
      from: entryPointAddress,
      to: smartAccountAddr,
      data: innerExecute,
    });
    console.log("  Simulation OK");
  } catch (err) {
    const data = (err as { data?: string; info?: { error?: { data?: string } } }).data
      ?? (err as { info?: { error?: { data?: string } } }).info?.error?.data
      ?? "0x";
    console.error(`  Simulation reverted: ${decodeRevert(data)}`);
    process.exit(1);
  }

  console.log("\nSending UserOp via EntryPoint.handleOps...");
  console.log(`  Nonce:      ${nonce}`);
  console.log(`  UserOpHash: ${userOpHash}`);

  const tx = await entryPoint.handleOps([op], userWallet.address);
  const receipt = await tx.wait();
  console.log(`  TX hash:    ${receipt!.hash}`);
  console.log(`  Block:      ${receipt!.blockNumber}`);
  console.log(`  Gas used:   ${receipt!.gasUsed}`);

  // Parse events
  const interfaces = [
    { name: "EntryPoint", iface: IEntryPoint__factory.createInterface() },
    { name: "ICredisFactory", iface: ICredisFactory__factory.createInterface() },
    { name: "ICredis", iface: ICredis__factory.createInterface() },
    { name: "VaultProvider", iface: IVaultProvider__factory.createInterface() },
    { name: "IGratis", iface: IGratis__factory.createInterface() },
    { name: "IGratisPool", iface: IGratisPool__factory.createInterface() },
    { name: "ERC20", iface: IERC20__factory.createInterface() },
  ];

  let userOpSuccess: boolean | null = null;
  let userOpRevertReason: string | null = null;
  // The anadosis inserts this installment's reclaim commitment into the pool;
  // capture (commitment, leafIndex, root) so we can write the local reclaim
  // ticket for a later unpledge.
  let poolInsertion: { commitment: bigint; leafIndex: number; root: bigint } | null = null;

  console.log("\n=== Transaction Events ===");
  for (const log of receipt?.logs ?? []) {
    let parsed = false;
    for (const { name: contractName, iface } of interfaces) {
      try {
        const event = iface.parseLog({ topics: log.topics as string[], data: log.data });
        if (event) {
          console.log(`  [${contractName}] ${event.name}:`);
          const fragment = event.fragment;
          for (let i = 0; i < fragment.inputs.length; i++) {
            const paramName = fragment.inputs[i].name;
            const value = event.args[i];
            console.log(`    ${paramName}: ${value}`);
          }
          if (contractName === "EntryPoint" && event.args.userOpHash === userOpHash) {
            if (event.name === "UserOperationEvent") userOpSuccess = event.args.success;
            if (event.name === "UserOperationRevertReason") userOpRevertReason = event.args.revertReason;
          }
          if (contractName === "IGratisPool" && event.name === "CommitmentInserted") {
            poolInsertion = {
              commitment: BigInt(event.args.commitment),
              leafIndex: Number(event.args.leafIndex),
              root: BigInt(event.args.newRoot),
            };
          }
          parsed = true;
          break;
        }
      } catch {
        // Not from this interface
      }
    }
    if (!parsed) {
      console.log(`  [Unknown] address=${log.address} topics=${log.topics[0]}`);
    }
  }

  if (userOpSuccess === false) {
    console.error("\n!! UserOperation reverted — outer handleOps tx still succeeded (ERC-4337 swallows inner reverts).");
    if (userOpRevertReason && userOpRevertReason !== "0x") {
      console.error(`  Decoded: ${decodeRevert(userOpRevertReason)}`);
    } else {
      console.error("  No UserOperationRevertReason emitted (validation phase failure or zero-length revert data).");
    }
    process.exit(1);
  }

  // State after
  const after = await getState(token, credis, smartAccountAddr, underlyingVaultAddr);
  printState("State AFTER", after, erc20Meta, smartAccountAddr);

  // Diff
  console.log("\n=== CHANGES ===");
  const saErc20Diff = after.saErc20Balance - before.saErc20Balance;
  const vaultErc20Diff = after.vaultErc20Balance - before.vaultErc20Balance;
  console.log(`  SA ERC20:        ${formatTokenDiff(saErc20Diff, erc20Meta.decimals, erc20Meta.symbol)}`);
  console.log(`  Vault ERC20:     ${formatTokenDiff(vaultErc20Diff, erc20Meta.decimals, erc20Meta.symbol)}`);

  // The runtime appended this installment's reclaim commitment to the pool.
  // Write a ticket so `unpledge-gratis-fast` can prove membership and unlock
  // one tenth of the pledge immediately — no need to wait for the loan to
  // finish.
  if (poolInsertion) {
    if (poolInsertion.commitment !== reclaimCommitment) {
      throw new Error(
        `Poseidon parity mismatch: sent reclaim commitment ${fieldToHex32(reclaimCommitment)} but the on-chain CommitmentInserted recorded ${fieldToHex32(poolInsertion.commitment)}.`,
      );
    }
    const network = await provider.getNetwork();
    const reclaimTicketPath = writeTicket({
      denomId: anadosisDenom.id,
      secret: fieldToHex32(reclaimSecret),
      nullifierSecret: fieldToHex32(reclaimNullifierSecret),
      commitment: fieldToHex32(reclaimCommitment),
      leafIndex: poolInsertion.leafIndex,
      root: fieldToHex32(poolInsertion.root),
      blockNumber: receipt!.blockNumber,
      txHash: receipt!.hash,
      chainId: network.chainId.toString(),
      createdAt: new Date().toISOString(),
    });
    console.log(`\nReclaim ticket written: ${reclaimTicketPath}`);
    console.log(`  denom ${anadosisDenom.id} = ${formatToken(anadosisDenom.amount, 18, "GRATIS")} (one installment's share)`);
    console.log(`  leafIndex: ${poolInsertion.leafIndex}`);
    console.log("  Run `npm run unpledge-gratis-fast` to unlock this installment's gratis now.");
  } else {
    console.log("\nWARNING: no CommitmentInserted event observed — reclaim note not recorded.");
  }
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
