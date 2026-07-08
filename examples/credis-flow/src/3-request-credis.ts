import { ethers, Wallet } from "ethers";
import {
  IGratis__factory,
  ICredisFactory__factory,
  ICredis__factory,
  SmartAccountFactory__factory,
  IERC20__factory,
  IVaultProvider__factory,
  IGratisPool__factory,
} from "./contracts/index.js";
import {
  DEFAULT_GRATIS_ADDRESS,
  DEFAULT_GRATIS_POOL_ADDRESS,
  DEFAULT_CREDIS_ADDRESS,
  DEFAULT_CREDIS_FACTORY_ADDRESS,
  GRATIS_DENOMINATIONS,
  TokenMeta,
  formatToken,
  formatTokenMeta,
  formatTokenDiff,
  fetchTokenMeta,
  DEFAULT_ENV,
  loadEnv,
  requireEnv,
} from "./utils.js";
import {
  ACTION_REQUEST_CREDIS,
  buildMerkleProof,
  commitmentHash,
  fieldToHex32,
  nullifierHash,
  proveUnpledge,
  receiverBinding,
  toField,
} from "./shielded.js";
import {
  deleteTicket,
  findLatestTicket,
  readTicket,
  writeTicket,
  type Ticket,
} from "./ticket.js";

const SALT = 0n;

// Parse CLI args: [ticketPath?] [envName?]
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
const gratisPoolAddress = process.env["GRATIS_POOL_ADDRESS"] || DEFAULT_GRATIS_POOL_ADDRESS;
const credisFactoryAddress = process.env["CREDIS_FACTORY_ADDRESS"] || DEFAULT_CREDIS_FACTORY_ADDRESS;
const credisAddress = process.env["CREDIS_ADDRESS"] || DEFAULT_CREDIS_ADDRESS;
const smartAccountFactoryAddress = requireEnv("SMART_ACCOUNT_FACTORY_ADDRESS", envContext);
const vaultProviderAddress = requireEnv("VAULT_PROVIDER_ADDRESS", envContext);
const erc20Address = requireEnv("ERC20_ADDRESS", envContext);

interface State {
  gratisBalance: bigint;
  pledged: bigint;
  pledgedTotalSupply: bigint;
  smartAccountErc20Balance: bigint;
  poolLeafCount: bigint;
  poolRoot: bigint;
  nullifierSpent: boolean;
}

async function getState(
  gratis: ReturnType<typeof IGratis__factory.connect>,
  pool: ReturnType<typeof IGratisPool__factory.connect>,
  token: ReturnType<typeof IERC20__factory.connect>,
  smartAccountAddr: string,
  denomId: number,
  nullifier: bigint,
): Promise<State> {
  const [
    gratisBalance,
    pledged,
    pledgedTotalSupply,
    smartAccountErc20Balance,
    poolLeafCount,
    poolRoot,
    nullifierSpent,
  ] = await Promise.all([
    gratis.balanceOf(userAddress),
    gratis.pledgedOf(userAddress),
    gratis.pledgedTotalSupply(),
    token.balanceOf(smartAccountAddr),
    pool.leafCount(denomId),
    pool.currentRoot(denomId),
    pool.isSpent(nullifier),
  ]);
  return {
    gratisBalance,
    pledged,
    pledgedTotalSupply,
    smartAccountErc20Balance,
    poolLeafCount: BigInt(poolLeafCount),
    poolRoot: BigInt(poolRoot),
    nullifierSpent,
  };
}

function printState(label: string, state: State, denomId: number, gratisMeta: TokenMeta, erc20Meta: TokenMeta) {
  console.log(`\n=== ${label} ===`);
  console.log(`  User Gratis balance:  ${formatTokenMeta(state.gratisBalance, gratisMeta)}`);
  console.log(`  User pledged:         ${formatTokenMeta(state.pledged, gratisMeta)}`);
  console.log(`  Pledged total:        ${formatTokenMeta(state.pledgedTotalSupply, gratisMeta)} (system-wide)`);
  console.log(`  Pool leafCount:       ${state.poolLeafCount} (denom ${denomId})`);
  console.log(`  Pool root:            ${fieldToHex32(state.poolRoot)}`);
  console.log(`  Nullifier spent:      ${state.nullifierSpent}`);
  console.log(`  Bundle account ERC20: ${formatTokenMeta(state.smartAccountErc20Balance, erc20Meta)}`);
}

function loadTicket(): { ticket: Ticket; path: string } {
  if (ticketPath) {
    return { ticket: readTicket(ticketPath), path: ticketPath };
  }
  const latest = findLatestTicket();
  if (!latest) {
    console.error(
      "No ticket file found under examples/credis-flow/tickets/. Run `npm run pledge-gratis` first or pass an explicit ticket path.",
    );
    process.exit(1);
  }
  return latest;
}

async function main() {
  const { ticket, path: usedTicketPath } = loadTicket();
  const denom = GRATIS_DENOMINATIONS.find((d) => d.id === ticket.denomId);
  if (!denom) {
    throw new Error(`Ticket denomId ${ticket.denomId} not in known ladder`);
  }

  const provider = new ethers.JsonRpcProvider(rpcUrl);
  const ccaWallet = new Wallet(ccaPrivateKey, provider);

  // Predict Bundle account address — this is the credis receiver and the
  // `target` slot folded into receiver_binding for ACTION_REQUEST_CREDIS.
  const saFactory = SmartAccountFactory__factory.connect(smartAccountFactoryAddress, provider);
  const smartAccountAddr = await saFactory.getAccountAddress(
    userAddress,
    ccaAddress,
    [erc20Address],
    [vaultProviderAddress],
    SALT,
  );

  // Connect contracts
  const credisFactory = ICredisFactory__factory.connect(credisFactoryAddress, ccaWallet);
  const gratis = IGratis__factory.connect(gratisAddress, provider);
  const gratisPool = IGratisPool__factory.connect(gratisPoolAddress, provider);
  const token = IERC20__factory.connect(erc20Address, provider);

  const [gratisMeta, erc20Meta, network] = await Promise.all([
    fetchTokenMeta(gratis),
    fetchTokenMeta(token),
    provider.getNetwork(),
  ]);

  // Decode ticket secrets.
  const secret = BigInt(ticket.secret);
  const nullifierSecret = BigInt(ticket.nullifierSecret);
  const commitment = BigInt(ticket.commitment);
  const denomId = ticket.denomId;
  const leafIndex = ticket.leafIndex;

  // Fresh reclaim commitment material — the credisfactory persists the
  // reclaim_commitment with the position and inserts it back into the pool
  // when the borrower finishes paying all anadosis installments. Only the
  // holder of (reclaimSecret, reclaimNullifierSecret) can later unpledge it.
  const reclaimSecret = toField(ethers.getBytes(ethers.hexlify(ethers.randomBytes(32))));
  const reclaimNullifierSecret = toField(ethers.getBytes(ethers.hexlify(ethers.randomBytes(32))));
  const reclaimCommitment = await commitmentHash(reclaimSecret, reclaimNullifierSecret, denomId);

  console.log("=== Request Credis (shielded) ===");
  console.log(`Env:               ${envName} (${envPath})`);
  console.log(`RPC:               ${rpcUrl}`);
  console.log(`Ticket:            ${usedTicketPath}`);
  console.log(`CCA:               ${ccaAddress}`);
  console.log(`User (owner):      ${userAddress}`);
  console.log(`CredisFactory:     ${credisFactoryAddress}`);
  console.log(`GratisPool:        ${gratisPoolAddress}`);
  console.log(`SA Factory:        ${smartAccountFactoryAddress}`);
  console.log(`Vault:             ${vaultProviderAddress}`);
  console.log(`ERC20:             ${erc20Address}`);
  console.log(`Bundle account:    ${smartAccountAddr}`);
  console.log(`Denom:             ${denomId} = ${formatToken(denom.amount, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`Commitment (old):  ${fieldToHex32(commitment)}`);
  console.log(`Leaf index (old):  ${leafIndex}`);
  console.log(`Reclaim secret:    ${fieldToHex32(reclaimSecret)}`);
  console.log(`Reclaim nullSecr:  ${fieldToHex32(reclaimNullifierSecret)}`);
  console.log(`Reclaim commit:    ${fieldToHex32(reclaimCommitment)}`);
  console.log(`Chain ID:          ${network.chainId}`);

  // Pre-flight: cheap on-chain checks before paying for proof generation.
  const nullifier = await nullifierHash(nullifierSecret);
  const [onChainRoot, onChainLeafCount, alreadySpent] = await Promise.all([
    gratisPool.currentRoot(denomId),
    gratisPool.leafCount(denomId),
    gratisPool.isSpent(nullifier),
  ]);

  if (Number(onChainLeafCount) <= leafIndex) {
    throw new Error(
      `Pool leafCount ${onChainLeafCount} <= ticket.leafIndex ${leafIndex} — ticket points past the tree`,
    );
  }
  if (alreadySpent) {
    deleteTicket(usedTicketPath);
    throw new Error(
      `Nullifier ${fieldToHex32(nullifier)} is already spent on-chain — ticket has been deleted (cannot be reused for credis)`,
    );
  }

  // Replay CommitmentInserted events to rebuild the local Merkle tree.
  console.log("\nReplaying CommitmentInserted events to rebuild the Merkle tree...");
  const filter = gratisPool.filters["CommitmentInserted(uint8,uint256,uint32,uint256)"](denomId);
  const events = await gratisPool.queryFilter(filter, 0, "latest");

  const commitments: bigint[] = [];
  for (const ev of events) {
    const idx = Number(ev.args.leafIndex);
    commitments[idx] = BigInt(ev.args.commitment);
  }
  for (let i = 0; i < commitments.length; i++) {
    if (commitments[i] === undefined) {
      throw new Error(`Missing CommitmentInserted event for leaf ${i} of denom ${denomId}`);
    }
  }
  console.log(`  Replayed ${commitments.length} commitments for denom ${denomId}`);

  if (commitments[leafIndex] !== commitment) {
    throw new Error(
      `Replayed commitment[${leafIndex}] = ${fieldToHex32(commitments[leafIndex])} does not match ticket commitment ${fieldToHex32(commitment)}`,
    );
  }

  const merkle = await buildMerkleProof(commitments, leafIndex);
  if (merkle.root !== BigInt(onChainRoot)) {
    throw new Error(
      `Locally-rebuilt root ${fieldToHex32(merkle.root)} does not match on-chain currentRoot ${fieldToHex32(BigInt(onChainRoot))} — likely Poseidon param drift.`,
    );
  }
  console.log(`  Rebuilt root matches on-chain currentRoot: ${fieldToHex32(merkle.root)}`);

  // receiver_binding for ACTION_REQUEST_CREDIS:
  //   target = bundleAccount, nonce = reclaim_commitment.
  // The reclaim_commitment is folded in so a mempool front-runner cannot swap
  // their own reclaim leg and capture the eventual unpledge. Matches
  // crates/core/gratispool/src/state.rs::receiver_binding and
  // crates/core/credisfactory/src/runtime.rs::request_credis.
  const binding = await receiverBinding(
    ACTION_REQUEST_CREDIS,
    smartAccountAddr,
    network.chainId,
    reclaimCommitment,
  );

  console.log("\nGenerating UltraHonkKeccak spend proof (this typically takes 3-8s)...");
  const tProveStart = Date.now();
  const proof = await proveUnpledge({
    secret,
    nullifierSecret,
    denomId,
    merklePath: merkle.siblings,
    merkleIndex: leafIndex,
    merkleRoot: merkle.root,
    nullifierHashValue: nullifier,
    receiverBindingValue: binding,
  });
  console.log(`  Proof generated in ${((Date.now() - tProveStart) / 1000).toFixed(2)}s (${proof.length} bytes)`);

  // State before
  const before = await getState(gratis, gratisPool, token, smartAccountAddr, denomId, nullifier);
  printState("State BEFORE", before, denomId, gratisMeta, erc20Meta);

  // Request credis
  console.log("\nSending requestCredis tx...");
  // requestCredis(asset, bundleAccount, args): the asset self-reports its ISO
  // currency via isoCode(), which the factory uses to pin the refinancing rate.
  const tx = await credisFactory.requestCredis(
    erc20Address,
    smartAccountAddr,
    {
      merkleRoot: merkle.root,
      nullifierHash: nullifier,
      denomId,
      receiverBinding: binding,
      proof: ethers.hexlify(proof),
      reclaimCommitment,
    },
  );
  console.log(`  TX hash: ${tx.hash}`);
  const receipt = await tx.wait();
  if (!receipt) throw new Error("requestCredis tx receipt missing");
  console.log(`  Block:   ${receipt.blockNumber}`);
  console.log(`  Gas:     ${receipt.gasUsed}`);

  // Parse all events from the transaction
  const credisIface = ICredis__factory.createInterface();
  const interfaces = [
    { name: "ICredisFactory", iface: ICredisFactory__factory.createInterface() },
    { name: "ICredis", iface: credisIface },
    { name: "VaultProvider", iface: IVaultProvider__factory.createInterface() },
    { name: "IGratis", iface: IGratis__factory.createInterface() },
    { name: "IGratisPool", iface: IGratisPool__factory.createInterface() },
    { name: "ERC20", iface: IERC20__factory.createInterface() },
  ];

  let createdPositionId: bigint | null = null;

  console.log("\n=== Transaction Events ===");
  for (const log of receipt.logs ?? []) {
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
          if (event.name === "PositionCreated") {
            createdPositionId = event.args[0];
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

  if (createdPositionId !== null) {
    console.log(`\n=== Created Position ID: ${createdPositionId} ===`);

    // Read back the pinned Anadosis terms: principal (disbursed loan) vs.
    // total debt (principal + refinancing-rate markup) and the issuance currency.
    const credis = ICredis__factory.connect(credisAddress, provider);
    const position = await credis.getPosition(createdPositionId);
    console.log(`  Issuance currency (ISO 4217): ${position.issuanceCurrency}`);
    console.log(`  Refinancing rate (1e18):      ${position.refinancingRate}`);
    console.log(`  Credis principal:             ${position.credisPrincipal}`);
    console.log(`  Total Anadosis debt:          ${position.totalAnadosisAmount}`);
  }

  // State after
  const after = await getState(gratis, gratisPool, token, smartAccountAddr, denomId, nullifier);
  printState("State AFTER", after, denomId, gratisMeta, erc20Meta);

  // Diff
  console.log("\n=== CHANGES ===");
  console.log(`  Gratis balance:     ${formatTokenDiff(after.gratisBalance - before.gratisBalance, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged:            ${formatTokenDiff(after.pledged - before.pledged, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  Pledged total:      ${formatTokenDiff(after.pledgedTotalSupply - before.pledgedTotalSupply, gratisMeta.decimals, gratisMeta.symbol)}`);
  console.log(`  SA ERC20 balance:   ${formatTokenDiff(after.smartAccountErc20Balance - before.smartAccountErc20Balance, erc20Meta.decimals, erc20Meta.symbol)}`);
  console.log(`  Nullifier spent:    ${before.nullifierSpent} -> ${after.nullifierSpent}`);

  // The original ticket's secret is now useless — its nullifier is on-chain.
  deleteTicket(usedTicketPath);
  console.log(`\nOriginal ticket deleted: ${usedTicketPath}`);

  // Persist the reclaim secret material. The reclaim_commitment will only be
  // appended to the pool when the borrower finishes all anadosis installments,
  // so leafIndex/root are unknown at this point — use sentinels and let the
  // unpledge flow rediscover them from on-chain CommitmentInserted events.
  const reclaimTicketPath = writeTicket({
    denomId,
    secret: fieldToHex32(reclaimSecret),
    nullifierSecret: fieldToHex32(reclaimNullifierSecret),
    commitment: fieldToHex32(reclaimCommitment),
    leafIndex: -1, // sentinel: not yet inserted into the pool
    root: "0x" + "00".repeat(32),
    blockNumber: receipt.blockNumber,
    txHash: receipt.hash,
    chainId: network.chainId.toString(),
    createdAt: new Date().toISOString(),
  });
  console.log(`Reclaim ticket written: ${reclaimTicketPath}`);
  console.log("  (leafIndex = -1 until the credis position completes and pay_anadosis inserts the reclaim commitment.)");
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
