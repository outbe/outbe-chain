import { task } from "hardhat/config";
import {
  createPublicClient,
  createWalletClient,
  encodePacked,
  getContract,
  http,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { addressToBytes32, getEnvRpcAndPk, makeChain } from "../../scripts/shared/layerzero.js";
import { getNetworkName } from "../../scripts/shared/taskUtils.js";
import { loadAbi } from "../../scripts/shared/abi.js";

type WireViem = {
  getContractAt: (name: string, address: `0x${string}`) => Promise<unknown>;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  getPublicClient: () => Promise<any>;
};

/** viem read/write facade for the wire network, built from its RPC + key, with ABIs from abi-export. */
async function getViemForWire(hre: unknown): Promise<WireViem> {
  const networkName = getNetworkName(hre);
  const { rpc, pk } = getEnvRpcAndPk(networkName);
  if (!pk) throw new Error(`Private key required for ${networkName}`);
  const chain = makeChain(networkName, rpc);
  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport });
  const walletClient = createWalletClient({ account, chain, transport });
  return {
    getContractAt: async (name: string, address: `0x${string}`) =>
      getContract({ address, abi: loadAbi(name), client: { public: publicClient, wallet: walletClient } }),
    getPublicClient: async () => publicClient,
  };
}

/** Send a write tx and wait for its receipt before the next dependent call. */
async function sendAndWait(
  viem: WireViem,
  writeFn: () => Promise<`0x${string}`>,
): Promise<`0x${string}`> {
  const hash = await writeFn();
  const publicClient = await viem.getPublicClient();
  await publicClient.waitForTransactionReceipt({ hash });
  return hash;
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
  const viem = await getViemForWire(hre);
  
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
  const viem = await getViemForWire(hre);
  
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

  const viem = await getViemForWire(hre);

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
// ONFT1155AdapterBatch Wire (grant SYSTEM_RELAYER_ROLE)
// ============================================================================

interface ONFTBatchAdapterWireArgs {
  batchAdapterContract: string;
  targetMessenger: string;
}

const onftBatchAdapterWireAction = async (args: ONFTBatchAdapterWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

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
// Precompile-caller Wire — grant roles to the EVM frames that initiate the
// gated calls: the begin-block system caller (auction stage sends + qualify/call
// mark sends) and the Desis precompile (inbound clearAuction issuance, where
// createSeries + issuance-instructions run in-process).
// ============================================================================

interface SystemGrantRolesArgs {
  bridgeContract: string;
  intexContract: string;
  systemAddress: string;
  desisContract: string;
}

const systemGrantRolesAction = async (args: SystemGrantRolesArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  if (!args.bridgeContract || args.bridgeContract === "null") {
    throw new Error("bridgeContract (OriginMessenger) is required");
  }
  if (!args.intexContract || args.intexContract === "null") {
    throw new Error("intexContract (IntexNFT1155) is required");
  }
  if (!args.systemAddress || args.systemAddress === "null") {
    throw new Error("systemAddress (OUTBE_SYSTEM_TX_ADDRESS) is required");
  }
  if (!args.desisContract || args.desisContract === "null") {
    throw new Error("desisContract (Desis precompile) is required");
  }
  const systemAddress = args.systemAddress as `0x${string}`;
  const desisAddress = args.desisContract as `0x${string}`;

  console.log(`Granting precompile-caller roles...`);
  console.log(`  SystemCaller:    ${systemAddress}`);
  console.log(`  Desis:           ${desisAddress}`);
  console.log(`  OriginMessenger: ${args.bridgeContract}`);
  console.log(`  IntexNFT1155:    ${args.intexContract}`);

  const messenger = (await viem.getContractAt(
    "OriginMessenger",
    args.bridgeContract as `0x${string}`
  )) as {
    read: {
      DESIS_ROLE: () => Promise<`0x${string}`>;
      INTEX_FACTORY_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const intex = (await viem.getContractAt(
    "IntexNFT1155",
    args.intexContract as `0x${string}`
  )) as {
    read: {
      RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const grantOnMessenger = async (label: string, role: `0x${string}`, addr: `0x${string}`) => {
    if (await messenger.read.hasRole([role, addr])) {
      console.log(`✅ OriginMessenger: ${addr} already has ${label}`);
    } else {
      const tx = await sendAndWait(viem, () => messenger.write.grantRole([role, addr]));
      console.log(`✅ OriginMessenger: ${label} -> ${addr}. Tx: ${tx}`);
    }
  };

  const grantOnIntex = async (label: string, role: `0x${string}`, addr: `0x${string}`) => {
    if (await intex.read.hasRole([role, addr])) {
      console.log(`✅ IntexNFT1155: ${addr} already has ${label}`);
    } else {
      const tx = await sendAndWait(viem, () => intex.write.grantRole([role, addr]));
      console.log(`✅ IntexNFT1155: ${label} -> ${addr}. Tx: ${tx}`);
    }
  };

  const desisRole = await messenger.read.DESIS_ROLE();
  const intexFactoryRole = await messenger.read.INTEX_FACTORY_ROLE();
  const relayerRole = await intex.read.RELAYER_ROLE();

  // Begin-block caller: auction stage sends (DESIS_ROLE), qualify/call mark
  // sends to BNB (INTEX_FACTORY_ROLE on OriginMessenger), and the local NFT
  // markQualified / markCalled (RELAYER_ROLE) — all run from begin-block.
  await grantOnMessenger("DESIS_ROLE", desisRole, systemAddress);
  await grantOnMessenger("INTEX_FACTORY_ROLE", intexFactoryRole, systemAddress);
  await grantOnIntex("RELAYER_ROLE", relayerRole, systemAddress);

  // Desis precompile frame (inbound clearAuction): issuance-instructions
  // (INTEX_FACTORY_ROLE) + createSeries (RELAYER_ROLE) run in-process here.
  await grantOnMessenger("INTEX_FACTORY_ROLE", intexFactoryRole, desisAddress);
  await grantOnIntex("RELAYER_ROLE", relayerRole, desisAddress);
};

const systemGrantRoles = task(
  "outbe-system-grant-roles",
  "Grant precompile-caller roles: DESIS_ROLE + INTEX_FACTORY_ROLE to the begin-block system caller; INTEX_FACTORY_ROLE + RELAYER_ROLE to the Desis precompile"
)
  .addOption({
    name: "bridgeContract",
    description: "OriginMessenger contract address",
    defaultValue: "",
  })
  .addOption({
    name: "intexContract",
    description: "IntexNFT1155 contract address on Outbe",
    defaultValue: "",
  })
  .addOption({
    name: "systemAddress",
    description: "Outbe begin-block system caller (OUTBE_SYSTEM_TX_ADDRESS)",
    defaultValue: "",
  })
  .addOption({
    name: "desisContract",
    description: "Desis precompile address (inbound clearAuction issuance frame)",
    defaultValue: "",
  })
  .setAction(lazy(systemGrantRolesAction));

// ============================================================================
// IntexFactory: Assert RELAYER_ROLE on IntexNFT1155 (deploy-time invariant)
// ============================================================================

interface IntexFactoryAssertRelayerRoleArgs {
  intexContract: string;
  desisContract: string;
  systemAddress: string;
}

const intexFactoryAssertRelayerRoleAction = async (args: IntexFactoryAssertRelayerRoleArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  if (!args.intexContract || args.intexContract === "null") {
    throw new Error("intexContract (IntexNFT1155) is required to assert RELAYER_ROLE");
  }
  if (!args.desisContract || args.desisContract === "null") {
    throw new Error("desisContract (Desis precompile) is required to assert RELAYER_ROLE");
  }
  if (!args.systemAddress || args.systemAddress === "null") {
    throw new Error("systemAddress (OUTBE_SYSTEM_TX_ADDRESS) is required to assert RELAYER_ROLE");
  }
  const desisAddress = args.desisContract as `0x${string}`;
  const systemAddress = args.systemAddress as `0x${string}`;

  console.log(`Asserting RELAYER_ROLE on IntexNFT1155 for the issuance + mark callers...`);
  console.log(`  IntexNFT1155: ${args.intexContract}`);
  console.log(`  Desis:        ${desisAddress} (createSeries)`);
  console.log(`  SystemCaller: ${systemAddress} (markQualified / markCalled)`);

  const intex = (await viem.getContractAt(
    "IntexNFT1155",
    args.intexContract as `0x${string}`
  )) as {
    read: {
      RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
  };

  // createSeries runs in the inbound clearAuction frame (Desis precompile);
  // markQualified / markCalled run from begin-block (the system caller). Both
  // need RELAYER_ROLE.
  const role = await intex.read.RELAYER_ROLE();
  for (const addr of [desisAddress, systemAddress]) {
    if (!(await intex.read.hasRole([role, addr]))) {
      throw new Error(
        `${addr} does NOT hold RELAYER_ROLE on IntexNFT1155 ${args.intexContract}. ` +
          `Issuance (createSeries) or qualify / call (markQualified / markCalled) will revert. ` +
          `Grant it first: outbe-system-grant-roles --bridge-contract <messenger> --intex-contract <intex> --system-address <system> --desis-contract <desis>.`,
      );
    }
  }

  console.log(`✅ Desis precompile and system caller hold RELAYER_ROLE on IntexNFT1155`);
};

const intexFactoryAssertRelayerRole = task(
  "intex-factory-assert-relayer-role",
  "Assert the Desis precompile and the begin-block system caller hold RELAYER_ROLE on IntexNFT1155 (fails the deploy if missing)",
)
  .addOption({ name: "intexContract", description: "IntexNFT1155 contract address on Outbe", defaultValue: "" })
  .addOption({ name: "desisContract", description: "Desis precompile address (createSeries caller)", defaultValue: "" })
  .addOption({ name: "systemAddress", description: "Outbe begin-block system caller (markQualified / markCalled)", defaultValue: "" })
  .setAction(lazy(intexFactoryAssertRelayerRoleAction));

// ============================================================================
// TargetMessenger Proceeds Route (BNB) — OFT-send auction proceeds to the
// outbe OriginMessenger, with the lzCompose gas allotment.
// ============================================================================

/** Destination gas for the lzCompose distribute on outbe; tune from gas reports. */
const COMPOSE_GAS = 500_000;

interface ProceedsRouteWireArgs {
  bridgeContract: string;
  originMessenger: string;
}

const bnbProceedsRouteWireAction = async (args: ProceedsRouteWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);
  const receiver = addressToBytes32(args.originMessenger as `0x${string}`);
  // LZ type-3 options: executor (worker 1) lzCompose (type 3), index 0, gas COMPOSE_GAS, value 0.
  const options = encodePacked(
    ["uint16", "uint8", "uint16", "uint8", "uint16", "uint128"],
    [3, 1, 19, 3, 0, BigInt(COMPOSE_GAS)],
  );

  console.log(`Wiring TargetMessenger proceeds route...`);
  console.log(`  TargetMessenger: ${args.bridgeContract}`);
  console.log(`  OriginMessenger: ${args.originMessenger}`);

  const bridge = (await viem.getContractAt(
    "TargetMessenger",
    args.bridgeContract as `0x${string}`
  )) as {
    read: { proceedsRoute: () => Promise<[`0x${string}`, `0x${string}`]> };
    write: { setProceedsRoute: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`> };
  };

  const [currentReceiver] = await bridge.read.proceedsRoute();
  if (currentReceiver.toLowerCase() === receiver.toLowerCase()) {
    console.log(`✅ TargetMessenger proceeds route already set`);
    return;
  }

  const txHash = await sendAndWait(viem, () => bridge.write.setProceedsRoute([receiver, options]));
  console.log(`✅ TargetMessenger proceeds route set. Tx: ${txHash}`);
};

const bnbProceedsRouteWire = task("bnb-proceeds-route-wire", "Set TargetMessenger's auction-proceeds OFT route to the outbe OriginMessenger")
  .addOption({ name: "bridgeContract", description: "TargetMessenger contract address", defaultValue: "" })
  .addOption({ name: "originMessenger", description: "outbe OriginMessenger address (proceeds receiver)", defaultValue: "" })
  .setAction(lazy(bnbProceedsRouteWireAction));

// ============================================================================
// OriginMessenger Compose Route (outbe) — OFT adapter + WCOEN it unwraps.
// ============================================================================

interface ComposeRouteWireArgs {
  bridgeContract: string;
  oftAdapter: string;
  wcoen: string;
}

const outbeComposeRouteWireAction = async (args: ComposeRouteWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Wiring OriginMessenger compose route...`);
  console.log(`  OriginMessenger: ${args.bridgeContract}`);
  console.log(`  OFTAdapter: ${args.oftAdapter}`);
  console.log(`  WCOEN: ${args.wcoen}`);

  const bridge = (await viem.getContractAt(
    "OriginMessenger",
    args.bridgeContract as `0x${string}`
  )) as {
    read: { composeRoute: () => Promise<[`0x${string}`, `0x${string}`]> };
    write: { setComposeRoute: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`> };
  };

  const [currentOft, currentWcoen] = await bridge.read.composeRoute();
  if (
    currentOft.toLowerCase() === args.oftAdapter.toLowerCase() &&
    currentWcoen.toLowerCase() === args.wcoen.toLowerCase()
  ) {
    console.log(`✅ OriginMessenger compose route already set`);
    return;
  }

  const txHash = await sendAndWait(viem, () =>
    bridge.write.setComposeRoute([args.oftAdapter as `0x${string}`, args.wcoen as `0x${string}`]),
  );
  console.log(`✅ OriginMessenger compose route set. Tx: ${txHash}`);
};

const outbeComposeRouteWire = task("outbe-compose-route-wire", "Set OriginMessenger's compose route (OFT adapter + WCOEN)")
  .addOption({ name: "bridgeContract", description: "OriginMessenger contract address", defaultValue: "" })
  .addOption({ name: "oftAdapter", description: "outbe OFT adapter authorized to deliver proceeds via lzCompose", defaultValue: "" })
  .addOption({ name: "wcoen", description: "WCOEN token unwrapped to native COEN", defaultValue: "" })
  .setAction(lazy(outbeComposeRouteWireAction));

// ============================================================================
// Export
// ============================================================================

export const wireTasks = [
  auctionWire.build(),
  escrowWire.build(),
  bnbBridgeWire.build(),
  bnbProceedsRouteWire.build(),
  onftBatchAdapterWire.build(),
  outbeBridgeWire.build(),
  outbeComposeRouteWire.build(),
  systemGrantRoles.build(),
  intexFactoryAssertRelayerRole.build(),
  settlementGrantRoles.build(),
  promisWire.build(),
];
