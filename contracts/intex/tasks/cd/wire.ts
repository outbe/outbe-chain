import { task } from "hardhat/config";
import {
  createPublicClient,
  createWalletClient,
  getContract,
  http,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { getEnvRpcAndPk, makeChain } from "../../scripts/shared/chains.js";
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
  nftBridgeContract: string;
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
// TargetRouter Wire
// ============================================================================

const bnbBridgeWireAction = async (args: BNBBridgeWireArgs, hre: unknown) => {
  const auction = (args.intexAuctionContract ?? "").trim();
  const intex = (args.intexContract ?? "").trim();
  const escrow = (args.escrowContract ?? "").trim();
  const nftBridge = (args.nftBridgeContract ?? "").trim();

  const empty: string[] = [];
  if (!auction) empty.push("--auction-contract");
  if (!intex) empty.push("--intex-contract");
  if (!escrow) empty.push("--escrow-contract");
  if (!nftBridge) empty.push("--nft-bridge-contract");
  if (empty.length > 0) {
    throw new Error(
      `TargetRouter wire requires non-empty addresses. Missing: ${empty.join(", ")}. ` +
        `Post-deploy workflow uses load-addresses from @outbe/intex-contracts package - ensure package has Auction, IntexNFT1155, EscrowAdapter, IntexNFT1155Bridge.`
    );
  }

  const viem = await getViemForWire(hre);

  console.log(`Wiring TargetRouter...`);
  console.log(`  Bridge: ${args.bridgeContract}`);
  console.log(`  Auction: ${auction}`);
  console.log(`  Intex: ${intex}`);
  console.log(`  Escrow: ${escrow}`);
  console.log(`  NftBridge: ${nftBridge}`);

  const bridge = (await viem.getContractAt(
    "TargetRouter",
    args.bridgeContract as `0x${string}`
  )) as {
    read: {
      auction: () => Promise<`0x${string}`>;
      intex: () => Promise<`0x${string}`>;
      escrowAdapter: () => Promise<`0x${string}`>;
      nftBridge: () => Promise<`0x${string}`>;
    };
    write: {
      wire: (args: [`0x${string}`, `0x${string}`, `0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const [currentAuction, currentIntex, currentEscrow, currentNftBridge] = await Promise.all([
    bridge.read.auction(),
    bridge.read.intex(),
    bridge.read.escrowAdapter(),
    bridge.read.nftBridge(),
  ]);

  const allMatch =
    currentAuction.toLowerCase() === auction.toLowerCase() &&
    currentIntex.toLowerCase() === intex.toLowerCase() &&
    currentEscrow.toLowerCase() === escrow.toLowerCase() &&
    currentNftBridge.toLowerCase() === nftBridge.toLowerCase();

  if (allMatch) {
    console.log(`✅ TargetRouter already wired to these contracts`);
    return;
  }

  if (currentAuction !== "0x0000000000000000000000000000000000000000") {
    const changed = [
      currentAuction.toLowerCase() !== auction.toLowerCase() && "auction",
      currentIntex.toLowerCase() !== intex.toLowerCase() && "intex",
      currentEscrow.toLowerCase() !== escrow.toLowerCase() && "escrow",
      currentNftBridge.toLowerCase() !== nftBridge.toLowerCase() && "nftBridge",
    ].filter(Boolean);
    console.log(`🔄 Rewiring TargetRouter (changed: ${changed.join(", ")})`);
  }

  const txHash = await sendAndWait(viem, () =>
    bridge.write.wire([
      auction as `0x${string}`,
      intex as `0x${string}`,
      escrow as `0x${string}`,
      nftBridge as `0x${string}`,
    ]),
  );
  console.log(`✅ TargetRouter wired. Tx: ${txHash}`);
};

const bnbBridgeWire = task("bnb-bridge-wire", "Wire TargetRouter to Auction, Intex, EscrowAdapter, and IntexNFT1155Bridge")
  .addOption({
    name: "bridgeContract",
    description: "TargetRouter contract address",
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
    name: "nftBridgeContract",
    description: "IntexNFT1155Bridge contract address",
    defaultValue: "",
  })
  .setAction(lazy(bnbBridgeWireAction));

// ============================================================================
// OriginRouter Wire
// ============================================================================

const outbeBridgeWireAction = async (args: OutbeBridgeWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);
  
  console.log(`Wiring OriginRouter...`);
  console.log(`  Bridge: ${args.bridgeContract}`);
  console.log(`  Desis: ${args.desisContract}`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);

  const bridge = (await viem.getContractAt(
    "OriginRouter",
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
    console.log(`✅ OriginRouter already wired to this Desis + IntexFactory`);
    return;
  }

  if (currentDesis !== ZERO) {
    console.log(`🔄 Rewiring OriginRouter`);
    if (!desisMatch) console.log(`   desis: ${currentDesis} -> ${args.desisContract}`);
    if (!intexFactoryMatch) console.log(`   intexFactory: ${currentIntexFactory} -> ${args.intexFactoryContract}`);
  }

  const txHash = await sendAndWait(viem, () =>
    bridge.write.wire([
      args.desisContract as `0x${string}`,
      args.intexFactoryContract as `0x${string}`,
    ]),
  );
  console.log(`✅ OriginRouter wired. Tx: ${txHash}`);
};

const outbeBridgeWire = task("outbe-bridge-wire", "Wire OriginRouter to Desis + IntexFactory")
  .addOption({
    name: "bridgeContract",
    description: "OriginRouter contract address",
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
// IntexNFT1155Bridge Wire (grant SYSTEM_RELAYER_ROLE)
// ============================================================================

interface NftBridgeWireArgs {
  batchAdapterContract: string;
  targetRouter: string;
}

const nftBridgeWireAction = async (args: NftBridgeWireArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);

  console.log(`Wiring IntexNFT1155Bridge (grant SYSTEM_RELAYER_ROLE)...`);
  console.log(`  BatchAdapter: ${args.batchAdapterContract}`);
  console.log(`  TargetRouter: ${args.targetRouter}`);

  const adapter = (await viem.getContractAt(
    "IntexNFT1155Bridge",
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
  const alreadyGranted = await adapter.read.hasRole([role, args.targetRouter as `0x${string}`]);

  if (alreadyGranted) {
    console.log(`✅ SYSTEM_RELAYER_ROLE already granted to TargetRouter`);
    return;
  }

  const txHash = await sendAndWait(viem, () =>
    adapter.write.grantRole([role, args.targetRouter as `0x${string}`]),
  );
  console.log(`✅ IntexNFT1155Bridge wired. Tx: ${txHash}`);
};

const nftBridgeWire = task("nft-bridge-wire", "Grant SYSTEM_RELAYER_ROLE on IntexNFT1155Bridge to TargetRouter")
  .addOption({
    name: "batchAdapterContract",
    description: "IntexNFT1155Bridge contract address",
    defaultValue: "",
  })
  .addOption({
    name: "targetRouter",
    description: "TargetRouter contract address",
    defaultValue: "",
  })
  .setAction(lazy(nftBridgeWireAction));

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
    throw new Error("bridgeContract (OriginRouter) is required");
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
  console.log(`  OriginRouter: ${args.bridgeContract}`);
  console.log(`  IntexNFT1155:    ${args.intexContract}`);

  const router = (await viem.getContractAt(
    "OriginRouter",
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

  const grantOnRouter = async (label: string, role: `0x${string}`, addr: `0x${string}`) => {
    if (await router.read.hasRole([role, addr])) {
      console.log(`✅ OriginRouter: ${addr} already has ${label}`);
    } else {
      const tx = await sendAndWait(viem, () => router.write.grantRole([role, addr]));
      console.log(`✅ OriginRouter: ${label} -> ${addr}. Tx: ${tx}`);
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

  const desisRole = await router.read.DESIS_ROLE();
  const intexFactoryRole = await router.read.INTEX_FACTORY_ROLE();
  const relayerRole = await intex.read.RELAYER_ROLE();

  // Begin-block caller: auction stage sends (DESIS_ROLE), qualify/call mark
  // sends to BNB (INTEX_FACTORY_ROLE on OriginRouter), and the local NFT
  // markQualified / markCalled (RELAYER_ROLE) — all run from begin-block.
  await grantOnRouter("DESIS_ROLE", desisRole, systemAddress);
  await grantOnRouter("INTEX_FACTORY_ROLE", intexFactoryRole, systemAddress);
  await grantOnIntex("RELAYER_ROLE", relayerRole, systemAddress);

  // Desis precompile frame (inbound clearAuction): issuance-instructions
  // (INTEX_FACTORY_ROLE) + createSeries (RELAYER_ROLE) run in-process here.
  await grantOnRouter("INTEX_FACTORY_ROLE", intexFactoryRole, desisAddress);
  await grantOnIntex("RELAYER_ROLE", relayerRole, desisAddress);
};

const systemGrantRoles = task(
  "outbe-system-grant-roles",
  "Grant precompile-caller roles: DESIS_ROLE + INTEX_FACTORY_ROLE to the begin-block system caller; INTEX_FACTORY_ROLE + RELAYER_ROLE to the Desis precompile"
)
  .addOption({
    name: "bridgeContract",
    description: "OriginRouter contract address",
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
          `Grant it first: outbe-system-grant-roles --bridge-contract <router> --intex-contract <intex> --system-address <system> --desis-contract <desis>.`,
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
// Grant RELAYER_ROLE (inbound-delivery caller)
// ============================================================================

interface GrantRelayerRoleArgs {
  token: string;
  adapter: string;
  contract: string;
}

const grantRelayerRoleAction = async (args: GrantRelayerRoleArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);
  const contractName = args.contract || "IntexNFT1155";

  console.log(`Granting RELAYER_ROLE on ${contractName} @ ${args.token} to ${args.adapter}...`);

  const token = (await viem.getContractAt(contractName, args.token as `0x${string}`)) as {
    read: {
      RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await token.read.RELAYER_ROLE();
  if (await token.read.hasRole([role, args.adapter as `0x${string}`])) {
    console.log("✅ RELAYER_ROLE already granted");
    return;
  }

  const txHash = await sendAndWait(viem, () => token.write.grantRole([role, args.adapter as `0x${string}`]));
  console.log(`✅ RELAYER_ROLE granted. Tx: ${txHash}`);
};

const grantRelayerRole = task(
  "grant-relayer-role",
  "Grant RELAYER_ROLE on an app contract (IntexAuction, EscrowAdapter, IntexNFT1155) to a bridge client",
)
  .addOption({ name: "token", description: "App contract that gates inbound calls by RELAYER_ROLE", defaultValue: "" })
  .addOption({ name: "adapter", description: "Bridge client to grant RELAYER_ROLE to (router or NFT bridge)", defaultValue: "" })
  .addOption({ name: "contract", description: "App contract name: IntexAuction | EscrowAdapter | IntexNFT1155 (default: IntexNFT1155)", defaultValue: "" })
  .setAction(lazy(grantRelayerRoleAction));

// ============================================================================
// Grant SYSTEM_RELAYER_ROLE (holder-migration system bridge)
// ============================================================================

const grantSystemRelayerRoleAction = async (args: GrantRelayerRoleArgs, hre: unknown) => {
  const viem = await getViemForWire(hre);
  const contractName = args.contract || "IntexNFT1155";

  console.log(`Granting SYSTEM_RELAYER_ROLE on ${contractName} @ ${args.token} to ${args.adapter}...`);

  const token = (await viem.getContractAt(contractName, args.token as `0x${string}`)) as {
    read: {
      SYSTEM_RELAYER_ROLE: () => Promise<`0x${string}`>;
      hasRole: (args: [`0x${string}`, `0x${string}`]) => Promise<boolean>;
    };
    write: {
      grantRole: (args: [`0x${string}`, `0x${string}`]) => Promise<`0x${string}`>;
    };
  };

  const role = await token.read.SYSTEM_RELAYER_ROLE();
  if (await token.read.hasRole([role, args.adapter as `0x${string}`])) {
    console.log("✅ SYSTEM_RELAYER_ROLE already granted");
    return;
  }

  const txHash = await sendAndWait(viem, () => token.write.grantRole([role, args.adapter as `0x${string}`]));
  console.log(`✅ SYSTEM_RELAYER_ROLE granted. Tx: ${txHash}`);
};

const grantSystemRelayerRole = task(
  "grant-system-relayer-role",
  "Grant SYSTEM_RELAYER_ROLE on IntexNFT1155 to the system holder-migration adapter (IntexNFT1155Bridge)",
)
  .addOption({ name: "token", description: "IntexNFT1155 contract address", defaultValue: "" })
  .addOption({ name: "adapter", description: "System bridge adapter to grant SYSTEM_RELAYER_ROLE to", defaultValue: "" })
  .addOption({ name: "contract", description: "Contract name (default: IntexNFT1155)", defaultValue: "" })
  .setAction(lazy(grantSystemRelayerRoleAction));

// ============================================================================
// Export
// ============================================================================

export const wireTasks = [
  auctionWire.build(),
  escrowWire.build(),
  bnbBridgeWire.build(),
  nftBridgeWire.build(),
  outbeBridgeWire.build(),
  systemGrantRoles.build(),
  intexFactoryAssertRelayerRole.build(),
  settlementGrantRoles.build(),
  promisWire.build(),
  grantRelayerRole.build(),
  grantSystemRelayerRole.build(),
];
