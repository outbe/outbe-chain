import { task } from "hardhat/config";
import {
  createPublicClient,
  createWalletClient,
  getContract,
  http,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import * as fs from "fs";
import { OUTBE_CHAINS } from "../../scripts/shared/layerzero.js";
import { getNetworkName } from "../../scripts/shared/taskUtils.js";

const ARTIFACT_PATHS: Record<string, string> = {
  OriginMessenger: "artifacts/contracts/outbe/OriginMessenger.sol/OriginMessenger.json",
  IntexNFT1155: "artifacts/contracts/shared/IntexNFT1155.sol/IntexNFT1155.json",
  Desis: "artifacts/contracts/outbe/Desis.sol/Desis.json",
  IntexFactory: "artifacts/contracts/outbe/IntexFactory.sol/IntexFactory.json",
};

function loadOutbeArtifact(name: string): { abi: unknown[] } {
  const p = ARTIFACT_PATHS[name];
  if (!p || !fs.existsSync(p)) throw new Error(`Artifact not found: ${name}. Run yarn compile.`);
  return JSON.parse(fs.readFileSync(p, "utf-8"));
}

/**
 * Wire-task viem facade.
 *
 * Both Outbe and non-Outbe paths expose the same minimum surface:
 *   - getContractAt(name, address) → contract instance with read/write
 *   - getPublicClient() → underlying viem PublicClient (used to wait for tx receipts)
 *
 * Waiting for receipts before issuing the next dependent call is mandatory:
 *   `escrow.write.wire(...)` returns a tx hash as soon as the tx hits the
 *   mempool — viem's simulation of the next call would otherwise read state
 *   that does not yet contain the previous tx, surfacing as `NotWired`,
 *   missing-role reverts, etc.
 */
type WireViem = {
  getContractAt: (name: string, address: `0x${string}`) => Promise<unknown>;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  getPublicClient: () => Promise<any>;
};

/** Outbe: viem + defineChain (hardhat-viem does not know 424242/512512). Others: network.connect() */
async function getViemForWire(hre: unknown): Promise<WireViem> {
  const networkName = getNetworkName(hre);
  const outbeNetworks = ["outbeTestnet", "outbeTestnetNew", "outbeDevnet", "outbePrivnet"] as const;
  type OutbeNetwork = (typeof outbeNetworks)[number];
  if (!outbeNetworks.includes(networkName as OutbeNetwork)) {
    const { viem } = await (hre as Hre).network.connect();
    return viem;
  }
  const chain = OUTBE_CHAINS[networkName as OutbeNetwork];
  const defaultRpcs: Record<string, string> = {
    outbeTestnet: "https://eth.testnet.outbe.net",
    outbeTestnetNew: "https://rpc.testnet.outbe.net",
    outbeDevnet: "https://eth.d.outbe.net",
    outbePrivnet: "https://eth.p.outbe.net",
  };
  const rpc = process.env.OUTBE_RPC_URL || defaultRpcs[networkName];
  const pk = process.env.OUTBE_PRIVATE_KEY;
  if (!pk) throw new Error("OUTBE_PRIVATE_KEY required for Outbe networks");
  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport });
  const walletClient = createWalletClient({ account, chain, transport });
  return {
    getContractAt: async (name: string, address: `0x${string}`) => {
      const { abi } = loadOutbeArtifact(name);
      return getContract({ address, abi: abi as unknown[], client: { public: publicClient, wallet: walletClient } });
    },
    getPublicClient: async () => publicClient,
  };
}

/**
 * Send a write tx and wait until it is mined.
 *
 * Use this for every `.write.*` call in this file. Returns the tx hash unchanged
 * so callers can keep logging it like before, but only after the receipt arrives.
 */
async function sendAndWait(
  viem: WireViem,
  writeFn: () => Promise<`0x${string}`>,
): Promise<`0x${string}`> {
  const hash = await writeFn();
  const publicClient = await viem.getPublicClient();
  await publicClient.waitForTransactionReceipt({ hash });
  return hash;
}

interface Hre {
  network: {
    connect: () => Promise<{
      viem: {
        getWalletClients: () => Promise<Array<{ account: { address: `0x${string}` } }>>;
        getContractAt: (name: string, address: `0x${string}`) => Promise<unknown>;
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        getPublicClient: () => Promise<any>;
      };
    }>;
  };
}

interface AuctionWireArgs {
  intexAuctionContract: string;
  escrowContract: string;
}

interface EscrowWireArgs {
  escrowContract: string;
  intexAuctionContract: string;
  compactContract: string;
  vaultProvider: string;
  paymentToken: string;
}

interface BNBBridgeWireArgs {
  bridgeContract: string;
  intexAuctionContract: string;
  intexContract: string;
  escrowContract: string;
  onftBatchAdapterContract: string;
}

interface OutbeBridgeWireArgs {
  bridgeContract: string;
  desisContract: string;
  intexFactoryContract: string;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const lazy = (fn: (args: any, hre: any) => Promise<void>) =>
  async () => ({ default: fn });

// ============================================================================
// Auction Wire
// ============================================================================

const auctionWireAction = async (args: AuctionWireArgs, hre: unknown) => {
  const { viem } = await (hre as Hre).network.connect();
  
  console.log(`Wiring Auction...`);
  console.log(`  Auction: ${args.intexAuctionContract}`);
  console.log(`  Escrow: ${args.escrowContract}`);

  const auction = (await viem.getContractAt(
    "IntexAuction",
    args.intexAuctionContract as `0x${string}`
  )) as {
    read: {
      escrowContract: () => Promise<`0x${string}`>;
    };
    write: {
      wire: (args: [`0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const currentEscrow = await auction.read.escrowContract();

  if (currentEscrow !== "0x0000000000000000000000000000000000000000") {
    if (currentEscrow.toLowerCase() === args.escrowContract.toLowerCase()) {
      console.log(`✅ Auction already wired to this Escrow`);
      return;
    }
    console.log(`🔄 Rewiring Auction (current: ${currentEscrow})`);
  }

  const txHash = await sendAndWait(viem, () =>
    auction.write.wire([args.escrowContract as `0x${string}`]),
  );
  console.log(`✅ Auction wired. Tx: ${txHash}`);
};

const auctionWire = task("auction-wire", "Wire Auction to EscrowAdapter")
  .addOption({
    name: "intexAuctionContract",
    description: "Auction contract address",
    defaultValue: "",
  })
  .addOption({
    name: "escrowContract",
    description: "EscrowAdapter contract address",
    defaultValue: "",
  })
  .setAction(lazy(auctionWireAction));

// ============================================================================
// EscrowAdapter Wire
// ============================================================================

const escrowWireAction = async (args: EscrowWireArgs, hre: unknown) => {
  const { viem } = await (hre as Hre).network.connect();
  
  console.log(`Wiring EscrowAdapter...`);
  console.log(`  Escrow: ${args.escrowContract}`);
  console.log(`  Auction: ${args.intexAuctionContract}`);
  console.log(`  Compact: ${args.compactContract}`);
  console.log(`  VaultProvider: ${args.vaultProvider}`);
  console.log(`  PaymentToken: ${args.paymentToken}`);

  const escrow = (await viem.getContractAt(
    "EscrowAdapter",
    args.escrowContract as `0x${string}`
  )) as {
    read: {
      intexAuctionContract: () => Promise<`0x${string}`>;
      compact: () => Promise<`0x${string}`>;
      vaultProvider: () => Promise<`0x${string}`>;
      paymentToken: () => Promise<`0x${string}`>;
    };
    write: {
      wire: (args: [`0x${string}`, `0x${string}`, `0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const [currentAuction, currentCompact, currentVaultProvider, currentStable] = await Promise.all([
    escrow.read.intexAuctionContract(),
    escrow.read.compact(),
    escrow.read.vaultProvider(),
    escrow.read.paymentToken(),
  ]);

  const allMatch =
    currentAuction.toLowerCase() === args.intexAuctionContract.toLowerCase() &&
    currentCompact.toLowerCase() === args.compactContract.toLowerCase() &&
    currentVaultProvider.toLowerCase() === args.vaultProvider.toLowerCase() &&
    currentStable.toLowerCase() === args.paymentToken.toLowerCase();

  if (allMatch) {
    console.log(`✅ EscrowAdapter already wired to these contracts`);
    return;
  }

  if (currentAuction !== "0x0000000000000000000000000000000000000000") {
    const changed = [
      currentAuction.toLowerCase() !== args.intexAuctionContract.toLowerCase() && "auction",
      currentCompact.toLowerCase() !== args.compactContract.toLowerCase() && "compact",
      currentVaultProvider.toLowerCase() !== args.vaultProvider.toLowerCase() && "vaultProvider",
      currentStable.toLowerCase() !== args.paymentToken.toLowerCase() && "paymentToken",
    ].filter(Boolean);
    console.log(`🔄 Rewiring EscrowAdapter (changed: ${changed.join(", ")})`);
  }

  const txHash = await sendAndWait(viem, () =>
    escrow.write.wire([
      args.intexAuctionContract as `0x${string}`,
      args.compactContract as `0x${string}`,
      args.vaultProvider as `0x${string}`,
      args.paymentToken as `0x${string}`,
    ]),
  );
  console.log(`✅ EscrowAdapter wired. Tx: ${txHash}`);
};

const escrowWire = task("escrow-wire", "Wire EscrowAdapter to Auction and external contracts")
  .addOption({
    name: "escrowContract",
    description: "EscrowAdapter contract address",
    defaultValue: "",
  })
  .addOption({
    name: "intexAuctionContract",
    description: "Auction contract address",
    defaultValue: "",
  })
  .addOption({
    name: "compactContract",
    description: "TheCompact contract address",
    defaultValue: "",
  })
  .addOption({
    name: "vaultProvider",
    description: "outbe-vault VaultProvider address (router that EscrowAdapter calls depositLiquidity on)",
    defaultValue: "",
  })
  .addOption({
    name: "paymentToken",
    description: "PaymentToken address",
    defaultValue: "",
  })
  .setAction(lazy(escrowWireAction));

// ============================================================================
// TargetMessenger Wire
// ============================================================================

const bnbBridgeWireAction = async (args: BNBBridgeWireArgs, hre: unknown) => {
  const auction = (args.intexAuctionContract ?? "").trim();
  const intex = (args.intexContract ?? "").trim();
  const escrow = (args.escrowContract ?? "").trim();
  const onftBatch = (args.onftBatchAdapterContract ?? "").trim();

  const empty: string[] = [];
  if (!auction) empty.push("--auction-contract");
  if (!intex) empty.push("--intex-contract");
  if (!escrow) empty.push("--escrow-contract");
  if (!onftBatch) empty.push("--onft-batch-adapter-contract");
  if (empty.length > 0) {
    throw new Error(
      `TargetMessenger wire requires non-empty addresses. Missing: ${empty.join(", ")}. ` +
        `Post-deploy workflow uses load-addresses from @outbe/intex-contracts package - ensure package has Auction, IntexNFT1155, EscrowAdapter, ONFT1155AdapterBatch.`
    );
  }

  const { viem } = await (hre as Hre).network.connect();

  console.log(`Wiring TargetMessenger...`);
  console.log(`  Bridge: ${args.bridgeContract}`);
  console.log(`  Auction: ${auction}`);
  console.log(`  Intex: ${intex}`);
  console.log(`  Escrow: ${escrow}`);
  console.log(`  ONFTBatchAdapter: ${onftBatch}`);

  const bridge = (await viem.getContractAt(
    "TargetMessenger",
    args.bridgeContract as `0x${string}`
  )) as {
    read: {
      auction: () => Promise<`0x${string}`>;
      intex: () => Promise<`0x${string}`>;
      escrowAdapter: () => Promise<`0x${string}`>;
      onftBatchAdapter: () => Promise<`0x${string}`>;
    };
    write: {
      wire: (args: [`0x${string}`, `0x${string}`, `0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const [currentAuction, currentIntex, currentEscrow, currentOnftBatch] = await Promise.all([
    bridge.read.auction(),
    bridge.read.intex(),
    bridge.read.escrowAdapter(),
    bridge.read.onftBatchAdapter(),
  ]);

  const allMatch =
    currentAuction.toLowerCase() === auction.toLowerCase() &&
    currentIntex.toLowerCase() === intex.toLowerCase() &&
    currentEscrow.toLowerCase() === escrow.toLowerCase() &&
    currentOnftBatch.toLowerCase() === onftBatch.toLowerCase();

  if (allMatch) {
    console.log(`✅ TargetMessenger already wired to these contracts`);
    return;
  }

  if (currentAuction !== "0x0000000000000000000000000000000000000000") {
    const changed = [
      currentAuction.toLowerCase() !== auction.toLowerCase() && "auction",
      currentIntex.toLowerCase() !== intex.toLowerCase() && "intex",
      currentEscrow.toLowerCase() !== escrow.toLowerCase() && "escrow",
      currentOnftBatch.toLowerCase() !== onftBatch.toLowerCase() && "onftBatchAdapter",
    ].filter(Boolean);
    console.log(`🔄 Rewiring TargetMessenger (changed: ${changed.join(", ")})`);
  }

  const txHash = await sendAndWait(viem, () =>
    bridge.write.wire([
      auction as `0x${string}`,
      intex as `0x${string}`,
      escrow as `0x${string}`,
      onftBatch as `0x${string}`,
    ]),
  );
  console.log(`✅ TargetMessenger wired. Tx: ${txHash}`);
};

const bnbBridgeWire = task("bnb-bridge-wire", "Wire TargetMessenger to Auction, Intex, EscrowAdapter, and ONFT1155AdapterBatch")
  .addOption({
    name: "bridgeContract",
    description: "TargetMessenger contract address",
    defaultValue: "",
  })
  .addOption({
    name: "intexAuctionContract",
    description: "Auction contract address",
    defaultValue: "",
  })
  .addOption({
    name: "intexContract",
    description: "IntexNFT1155 contract address",
    defaultValue: "",
  })
  .addOption({
    name: "escrowContract",
    description: "EscrowAdapter contract address",
    defaultValue: "",
  })
  .addOption({
    name: "onftBatchAdapterContract",
    description: "ONFT1155AdapterBatch contract address",
    defaultValue: "",
  })
  .setAction(lazy(bnbBridgeWireAction));

// ============================================================================
// OriginMessenger Wire
// ============================================================================

const outbeBridgeWireAction = async (args: OutbeBridgeWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);
  
  console.log(`Wiring OriginMessenger...`);
  console.log(`  Bridge: ${args.bridgeContract}`);
  console.log(`  Desis: ${args.desisContract}`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);

  const bridge = (await viem.getContractAt(
    "OriginMessenger",
    args.bridgeContract as `0x${string}`
  )) as {
    read: {
      desis: () => Promise<`0x${string}`>;
      intexFactory: () => Promise<`0x${string}`>;
    };
    write: {
      wire: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const ZERO = "0x0000000000000000000000000000000000000000";
  const [currentDesis, currentIntexFactory] = await Promise.all([
    bridge.read.desis(),
    bridge.read.intexFactory(),
  ]);

  const desisMatch = currentDesis.toLowerCase() === args.desisContract.toLowerCase();
  const intexFactoryMatch = currentIntexFactory.toLowerCase() === args.intexFactoryContract.toLowerCase();

  if (desisMatch && intexFactoryMatch) {
    console.log(`✅ OriginMessenger already wired to this Desis + IntexFactory`);
    return;
  }

  if (currentDesis !== ZERO) {
    console.log(`🔄 Rewiring OriginMessenger`);
    if (!desisMatch) console.log(`   desis: ${currentDesis} -> ${args.desisContract}`);
    if (!intexFactoryMatch) console.log(`   intexFactory: ${currentIntexFactory} -> ${args.intexFactoryContract}`);
  }

  const txHash = await sendAndWait(viem, () =>
    bridge.write.wire([
      args.desisContract as `0x${string}`,
      args.intexFactoryContract as `0x${string}`,
    ]),
  );
  console.log(`✅ OriginMessenger wired. Tx: ${txHash}`);
};

const outbeBridgeWire = task("outbe-bridge-wire", "Wire OriginMessenger to Desis + IntexFactory")
  .addOption({
    name: "bridgeContract",
    description: "OriginMessenger contract address",
    defaultValue: "",
  })
  .addOption({
    name: "desisContract",
    description: "Desis contract address",
    defaultValue: "",
  })
  .addOption({
    name: "intexFactoryContract",
    description: "IntexFactory contract address",
    defaultValue: "",
  })
  .setAction(lazy(outbeBridgeWireAction));

// ============================================================================
// Desis Wire
// ============================================================================

interface DesisWireArgs {
  desisContract: string;
  bridgeAdapter: string;
  promisLimitContract: string;
  intexFactoryContract: string;
}

const desisWireAction = async (args: DesisWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);
  
  console.log(`Wiring Desis...`);
  console.log(`  Desis: ${args.desisContract}`);
  console.log(`  OriginMessenger: ${args.bridgeAdapter}`);
  console.log(`  PromisLimit: ${args.promisLimitContract}`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);

  const desis = (await viem.getContractAt(
    "Desis",
    args.desisContract as `0x${string}`
  )) as {
    read: {
      messengerAdapter: () => Promise<`0x${string}`>;
      promisLimit: () => Promise<`0x${string}`>;
      intexFactory: () => Promise<`0x${string}`>;
    };
    write: {
      wire: (args: [`0x${string}`, `0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const ZERO = "0x0000000000000000000000000000000000000000";
  const [currentAdapter, currentPromisLimit, currentIntexFactory] = await Promise.all([
    desis.read.messengerAdapter(),
    desis.read.promisLimit(),
    desis.read.intexFactory(),
  ]);

  const adapterMatch = currentAdapter.toLowerCase() === args.bridgeAdapter.toLowerCase();
  const promisLimitMatch = currentPromisLimit.toLowerCase() === args.promisLimitContract.toLowerCase();
  const intexFactoryMatch = currentIntexFactory.toLowerCase() === args.intexFactoryContract.toLowerCase();

  if (adapterMatch && promisLimitMatch && intexFactoryMatch) {
    console.log(`✅ Desis already wired to all dependencies`);
    return;
  }

  if (currentAdapter !== ZERO) {
    console.log(`🔄 Rewiring Desis`);
    if (!adapterMatch) console.log(`   messengerAdapter: ${currentAdapter} -> ${args.bridgeAdapter}`);
    if (!promisLimitMatch) console.log(`   promisLimit: ${currentPromisLimit} -> ${args.promisLimitContract}`);
    if (!intexFactoryMatch) console.log(`   intexFactory: ${currentIntexFactory} -> ${args.intexFactoryContract}`);
  }

  const txHash = await sendAndWait(viem, () =>
    desis.write.wire([
      args.bridgeAdapter as `0x${string}`,
      args.promisLimitContract as `0x${string}`,
      args.intexFactoryContract as `0x${string}`,
    ]),
  );
  console.log(`✅ Desis wired. Tx: ${txHash}`);
};

const desisWire = task("desis-wire", "Wire Desis to OriginMessenger, PromisLimit, and IntexFactory")
  .addOption({
    name: "desisContract",
    description: "Desis contract address",
    defaultValue: "",
  })
  .addOption({
    name: "bridgeAdapter",
    description: "OriginMessenger contract address",
    defaultValue: "",
  })
  .addOption({
    name: "promisLimitContract",
    description: "PromisLimit precompile address on Outbe",
    defaultValue: "",
  })
  .addOption({
    name: "intexFactoryContract",
    description: "IntexFactory contract address on Outbe",
    defaultValue: "",
  })
  .setAction(lazy(desisWireAction));

// ============================================================================
// ONFT1155AdapterBatch Wire (grant SYSTEM_RELAYER_ROLE)
// ============================================================================

interface ONFTBatchAdapterWireArgs {
  batchAdapterContract: string;
  targetMessenger: string;
}

const onftBatchAdapterWireAction = async (args: ONFTBatchAdapterWireArgs, hre: unknown) => {
  const { viem } = await (hre as Hre).network.connect();

  console.log(`Wiring ONFT1155AdapterBatch (grant SYSTEM_RELAYER_ROLE)...`);
  console.log(`  BatchAdapter: ${args.batchAdapterContract}`);
  console.log(`  TargetMessenger: ${args.targetMessenger}`);

  const adapter = (await viem.getContractAt(
    "ONFT1155AdapterBatch",
    args.batchAdapterContract as `0x${string}`
  )) as {
    read: {
      SYSTEM_RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await adapter.read.SYSTEM_RELAYER_ROLE();
  const alreadyGranted = await adapter.read.hasRole([role, args.targetMessenger as `0x${string}`]);

  if (alreadyGranted) {
    console.log(`✅ SYSTEM_RELAYER_ROLE already granted to TargetMessenger`);
    return;
  }

  const txHash = await sendAndWait(viem, () =>
    adapter.write.grantRole([role, args.targetMessenger as `0x${string}`]),
  );
  console.log(`✅ ONFT1155AdapterBatch wired. Tx: ${txHash}`);
};

const onftBatchAdapterWire = task("onft-batch-adapter-wire", "Grant SYSTEM_RELAYER_ROLE on ONFT1155AdapterBatch to TargetMessenger")
  .addOption({
    name: "batchAdapterContract",
    description: "ONFT1155AdapterBatch contract address",
    defaultValue: "",
  })
  .addOption({
    name: "targetMessenger",
    description: "TargetMessenger contract address",
    defaultValue: "",
  })
  .setAction(lazy(onftBatchAdapterWireAction));

// ============================================================================
// IntexFactory Grant Roles
// Grant SETTLEMENT_ROLE on IntexNFT1155 to IntexFactory so it can call
// `intex.settle(...)` and burn Issued / mint Settled tokens.
// ============================================================================

interface SettlementGrantRolesArgs {
  settlementContract: string;
  intexContract: string;
}

const settlementGrantRolesAction = async (args: SettlementGrantRolesArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Granting roles for IntexFactory...`);
  console.log(`  IntexFactory: ${args.settlementContract}`);
  console.log(`  IntexNFT1155: ${args.intexContract}`);

  const intex = (await viem.getContractAt(
    "IntexNFT1155",
    args.intexContract as `0x${string}`
  )) as {
    read: {
      SETTLEMENT_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const settlementRole = await intex.read.SETTLEMENT_ROLE();
  const hasIntexRole = await intex.read.hasRole([
    settlementRole,
    args.settlementContract as `0x${string}`,
  ]);
  if (hasIntexRole) {
    console.log(`✅ IntexNFT1155: IntexFactory already has SETTLEMENT_ROLE`);
  } else {
    const tx1 = await sendAndWait(viem, () =>
      intex.write.grantRole([
        settlementRole,
        args.settlementContract as `0x${string}`,
      ]),
    );
    console.log(`✅ IntexNFT1155: SETTLEMENT_ROLE granted to IntexFactory. Tx: ${tx1}`);
  }
};

const settlementGrantRoles = task(
  "settlement-grant-roles",
  "Grant SETTLEMENT_ROLE on IntexNFT1155 to IntexFactory"
)
  .addOption({
    name: "settlementContract",
    description: "IntexFactory contract address",
    defaultValue: "",
  })
  .addOption({
    name: "intexContract",
    description: "IntexNFT1155 contract address on Outbe",
    defaultValue: "",
  })
  .setAction(lazy(settlementGrantRolesAction));

// ============================================================================
// Desis: Grant METADOSIS_ROLE
// ============================================================================

interface DesisGrantMetadosisRoleArgs {
  desisContract: string;
  metadosisAddress: string;
}

const desisGrantMetadosisRoleAction = async (args: DesisGrantMetadosisRoleArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Granting METADOSIS_ROLE on Desis...`);
  console.log(`  Desis: ${args.desisContract}`);
  console.log(`  Metadosis: ${args.metadosisAddress}`);

  const desis = (await viem.getContractAt(
    "Desis",
    args.desisContract as `0x${string}`
  )) as {
    read: {
      METADOSIS_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await desis.read.METADOSIS_ROLE();
  const alreadyGranted = await desis.read.hasRole([role, args.metadosisAddress as `0x${string}`]);

  if (alreadyGranted) {
    console.log(`✅ METADOSIS_ROLE already granted`);
    return;
  }

  const txHash = await sendAndWait(viem, () =>
    desis.write.grantRole([role, args.metadosisAddress as `0x${string}`]),
  );
  console.log(`✅ METADOSIS_ROLE granted on Desis. Tx: ${txHash}`);
};

const desisGrantMetadosisRole = task("desis-grant-metadosis-role", "Grant METADOSIS_ROLE on Desis to a given address")
  .addOption({
    name: "desisContract",
    description: "Desis contract address",
    defaultValue: "",
  })
  .addOption({
    name: "metadosisAddress",
    description: "Address to grant METADOSIS_ROLE to",
    defaultValue: "",
  })
  .setAction(lazy(desisGrantMetadosisRoleAction));

// ============================================================================
// Promis-burner Wire (grant PROMIS_ROLE on IntexNFT1155 to IntexFactory)
// IntexFactory.minePromis calls intex.burnSettled, which is gated by PROMIS_ROLE.
// ============================================================================

interface PromisWireArgs {
  settlementContract: string;
  intexContract: string;
}

const promisWireAction = async (args: PromisWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Granting PROMIS_ROLE on IntexNFT1155...`);
  console.log(`  IntexFactory: ${args.settlementContract}`);
  console.log(`  IntexNFT1155: ${args.intexContract}`);

  const intex = (await viem.getContractAt(
    "IntexNFT1155",
    args.intexContract as `0x${string}`
  )) as {
    read: {
      PROMIS_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await intex.read.PROMIS_ROLE();
  const hasPromisRole = await intex.read.hasRole([role, args.settlementContract as `0x${string}`]);
  if (hasPromisRole) {
    console.log(`✅ IntexNFT1155: IntexFactory already has PROMIS_ROLE`);
  } else {
    const tx = await sendAndWait(viem, () =>
      intex.write.grantRole([role, args.settlementContract as `0x${string}`]),
    );
    console.log(`✅ IntexNFT1155: PROMIS_ROLE granted to IntexFactory. Tx: ${tx}`);
  }
};

const promisWire = task(
  "promis-wire",
  "Grant PROMIS_ROLE on IntexNFT1155 to IntexFactory (enables minePromis burn path)"
)
  .addOption({
    name: "settlementContract",
    description: "IntexFactory contract address",
    defaultValue: "",
  })
  .addOption({
    name: "intexContract",
    description: "IntexNFT1155 contract address on Outbe",
    defaultValue: "",
  })
  .setAction(lazy(promisWireAction));

// ============================================================================
// IntexFactory Wire (wire: messengerAdapter + intex)
// ============================================================================

interface IntexFactoryWireArgs {
  intexFactoryContract: string;
  bridgeAdapter: string;
  intexContract: string;
}

const intexFactoryWireAction = async (args: IntexFactoryWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Wiring IntexFactory...`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);
  console.log(`  OriginMessenger: ${args.bridgeAdapter}`);
  console.log(`  IntexNFT1155: ${args.intexContract}`);

  const factory = (await viem.getContractAt(
    "IntexFactory",
    args.intexFactoryContract as `0x${string}`
  )) as {
    read: {
      messengerAdapter: () => Promise<`0x${string}`>;
      intex: () => Promise<`0x${string}`>;
    };
    write: {
      wire: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const ZERO = "0x0000000000000000000000000000000000000000";
  const [currentAdapter, currentIntex] = await Promise.all([
    factory.read.messengerAdapter(),
    factory.read.intex(),
  ]);

  const adapterMatch = currentAdapter.toLowerCase() === args.bridgeAdapter.toLowerCase();
  const intexMatch = currentIntex.toLowerCase() === args.intexContract.toLowerCase();

  if (adapterMatch && intexMatch) {
    console.log(`✅ IntexFactory already wired to all dependencies`);
    return;
  }

  if (currentAdapter !== ZERO) {
    console.log(`🔄 Rewiring IntexFactory`);
    if (!adapterMatch) console.log(`   messengerAdapter: ${currentAdapter} -> ${args.bridgeAdapter}`);
    if (!intexMatch) console.log(`   intex: ${currentIntex} -> ${args.intexContract}`);
  }

  const txHash = await sendAndWait(viem, () =>
    factory.write.wire([
      args.bridgeAdapter as `0x${string}`,
      args.intexContract as `0x${string}`,
    ]),
  );
  console.log(`✅ IntexFactory wired. Tx: ${txHash}`);
};

const intexFactoryWire = task("intex-factory-wire", "Wire IntexFactory to OriginMessenger + IntexNFT1155")
  .addOption({ name: "intexFactoryContract", description: "IntexFactory contract address", defaultValue: "" })
  .addOption({ name: "bridgeAdapter", description: "OriginMessenger contract address", defaultValue: "" })
  .addOption({ name: "intexContract", description: "IntexNFT1155 contract address on Outbe", defaultValue: "" })
  .setAction(lazy(intexFactoryWireAction));

// ============================================================================
// IntexFactory: Grant DESIS_ROLE to Desis (clearAuction -> IntexFactory.issue)
// ============================================================================

interface IntexFactoryGrantDesisRoleArgs {
  intexFactoryContract: string;
  desisContract: string;
}

const intexFactoryGrantDesisRoleAction = async (args: IntexFactoryGrantDesisRoleArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Granting DESIS_ROLE on IntexFactory...`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);
  console.log(`  Desis: ${args.desisContract}`);

  const factory = (await viem.getContractAt(
    "IntexFactory",
    args.intexFactoryContract as `0x${string}`
  )) as {
    read: {
      DESIS_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await factory.read.DESIS_ROLE();
  const alreadyGranted = await factory.read.hasRole([role, args.desisContract as `0x${string}`]);

  if (alreadyGranted) {
    console.log(`✅ DESIS_ROLE already granted to Desis`);
    return;
  }

  const txHash = await sendAndWait(viem, () =>
    factory.write.grantRole([role, args.desisContract as `0x${string}`]),
  );
  console.log(`✅ DESIS_ROLE granted on IntexFactory to Desis. Tx: ${txHash}`);
};

const intexFactoryGrantDesisRole = task("intex-factory-grant-desis-role", "Grant DESIS_ROLE on IntexFactory to Desis")
  .addOption({ name: "intexFactoryContract", description: "IntexFactory contract address", defaultValue: "" })
  .addOption({ name: "desisContract", description: "Desis contract address", defaultValue: "" })
  .setAction(lazy(intexFactoryGrantDesisRoleAction));

// ============================================================================
// IntexFactory: Grant METADOSIS_ROLE (markSeriesQualified / markSeriesCalled)
// ============================================================================

interface IntexFactoryGrantMetadosisRoleArgs {
  intexFactoryContract: string;
  metadosisAddress: string;
}

const intexFactoryGrantMetadosisRoleAction = async (args: IntexFactoryGrantMetadosisRoleArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Granting METADOSIS_ROLE on IntexFactory...`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);
  console.log(`  Metadosis: ${args.metadosisAddress}`);

  const factory = (await viem.getContractAt(
    "IntexFactory",
    args.intexFactoryContract as `0x${string}`
  )) as {
    read: {
      METADOSIS_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await factory.read.METADOSIS_ROLE();
  const alreadyGranted = await factory.read.hasRole([role, args.metadosisAddress as `0x${string}`]);

  if (alreadyGranted) {
    console.log(`✅ METADOSIS_ROLE already granted`);
    return;
  }

  const txHash = await sendAndWait(viem, () =>
    factory.write.grantRole([role, args.metadosisAddress as `0x${string}`]),
  );
  console.log(`✅ METADOSIS_ROLE granted on IntexFactory. Tx: ${txHash}`);
};

const intexFactoryGrantMetadosisRole = task("intex-factory-grant-metadosis-role", "Grant METADOSIS_ROLE on IntexFactory to a given address")
  .addOption({ name: "intexFactoryContract", description: "IntexFactory contract address", defaultValue: "" })
  .addOption({ name: "metadosisAddress", description: "Address to grant METADOSIS_ROLE to", defaultValue: "" })
  .setAction(lazy(intexFactoryGrantMetadosisRoleAction));

// ============================================================================
// IntexFactory: Assert RELAYER_ROLE on IntexNFT1155 (deploy-time invariant)
// ============================================================================

interface IntexFactoryAssertRelayerRoleArgs {
  intexContract: string;
  intexFactoryContract: string;
}

const intexFactoryAssertRelayerRoleAction = async (args: IntexFactoryAssertRelayerRoleArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  if (!args.intexContract || args.intexContract === "null") {
    throw new Error("intexContract (IntexNFT1155) is required to assert RELAYER_ROLE");
  }
  if (!args.intexFactoryContract || args.intexFactoryContract === "null") {
    throw new Error("intexFactoryContract is required to assert RELAYER_ROLE");
  }

  console.log(`Asserting IntexFactory holds RELAYER_ROLE on IntexNFT1155...`);
  console.log(`  IntexNFT1155: ${args.intexContract}`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);

  const intex = (await viem.getContractAt(
    "IntexNFT1155",
    args.intexContract as `0x${string}`
  )) as {
    read: {
      RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
  };

  const role = await intex.read.RELAYER_ROLE();
  const granted = await intex.read.hasRole([role, args.intexFactoryContract as `0x${string}`]);

  if (!granted) {
    throw new Error(
      `IntexFactory ${args.intexFactoryContract} does NOT hold RELAYER_ROLE on IntexNFT1155 ${args.intexContract}. ` +
        `issue / markSeriesQualified / markSeriesCalled (and Desis.clearAuction auto-continuation) will revert. ` +
        `Grant it first: lz:grant-bridge-role --token <intex> --adapter <factory>.`,
    );
  }

  console.log(`✅ IntexFactory holds RELAYER_ROLE on IntexNFT1155`);
};

const intexFactoryAssertRelayerRole = task(
  "intex-factory-assert-relayer-role",
  "Assert IntexFactory holds RELAYER_ROLE on IntexNFT1155 (fails the deploy if missing)",
)
  .addOption({ name: "intexContract", description: "IntexNFT1155 contract address on Outbe", defaultValue: "" })
  .addOption({ name: "intexFactoryContract", description: "IntexFactory contract address", defaultValue: "" })
  .setAction(lazy(intexFactoryAssertRelayerRoleAction));

// ============================================================================
// Export
// ============================================================================

export const wireTasks = [
  auctionWire.build(),
  escrowWire.build(),
  bnbBridgeWire.build(),
  onftBatchAdapterWire.build(),
  outbeBridgeWire.build(),
  desisWire.build(),
  desisGrantMetadosisRole.build(),
  intexFactoryWire.build(),
  intexFactoryGrantDesisRole.build(),
  intexFactoryGrantMetadosisRole.build(),
  intexFactoryAssertRelayerRole.build(),
  settlementGrantRoles.build(),
  promisWire.build(),
];
