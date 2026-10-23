import { getViemForWire, sendAndWait, type Address, type WireViem } from "./wireClient.js";

const ZERO = "0x0000000000000000000000000000000000000000";

type WiredContract = {
  read: Record<string, () => Promise<Address>>;
  write: { wire: (args: Address[]) => Promise<Address> };
};

interface AuctionWireArgs {
  intexAuctionContract: string;
  escrowContract: string;
}

interface EscrowWireArgs extends AuctionWireArgs {
  compactContract: string;
  paymentToken: string;
}

interface TargetBridgeWireArgs extends AuctionWireArgs {
  bridgeContract: string;
  intexContract: string;
}

export async function auctionWireAction(args: AuctionWireArgs, hre: unknown) {
  const viem = await getViemForWire(hre);
  console.log(`Wiring Auction...`);
  console.log(`  Auction: ${args.intexAuctionContract}`);
  console.log(`  Escrow: ${args.escrowContract}`);

  const auction = await viem.getContractAt("IntexAuction", args.intexAuctionContract as Address) as WiredContract;
  const currentEscrow = await auction.read.escrowContract();
  if (currentEscrow !== ZERO) {
    if (currentEscrow.toLowerCase() === args.escrowContract.toLowerCase()) {
      console.log(`[OK] Auction already wired to this Escrow`);
      return;
    }
    console.log(`[sync] Rewiring Auction (current: ${currentEscrow})`);
  }

  const txHash = await sendAndWait(viem, () => auction.write.wire([args.escrowContract as Address]));
  console.log(`[OK] Auction wired. Tx: ${txHash}`);
}

async function wireTriple(
  viem: WireViem,
  contract: WiredContract,
  config: { name: string; current: Address[]; desired: string[]; labels: string[] },
) {
  const { name, current, desired, labels } = config;
  const matches = (index: number) => current[index].toLowerCase() === desired[index].toLowerCase();
  const allMatch = matches(0) && matches(1) && matches(2);
  if (allMatch) {
    console.log(`[OK] ${name} already wired to these contracts`);
    return;
  }
  if (current[0] !== ZERO) {
    const changed = labels.map((label, index) => !matches(index) && label).filter(Boolean);
    console.log(`[sync] Rewiring ${name} (changed: ${changed.join(", ")})`);
  }
  const txHash = await sendAndWait(viem, () => contract.write.wire(desired as Address[]));
  console.log(`[OK] ${name} wired. Tx: ${txHash}`);
}

export async function escrowWireAction(args: EscrowWireArgs, hre: unknown) {
  const viem = await getViemForWire(hre);
  console.log(`Wiring EscrowAdapter...`);
  console.log(`  Escrow: ${args.escrowContract}`);
  console.log(`  Auction: ${args.intexAuctionContract}`);
  console.log(`  Compact: ${args.compactContract}`);
  console.log(`  PaymentToken: ${args.paymentToken}`);

  const escrow = await viem.getContractAt("EscrowAdapter", args.escrowContract as Address) as WiredContract;
  const current = await Promise.all([
    escrow.read.intexAuctionContract(), escrow.read.compact(), escrow.read.paymentToken(),
  ]);
  await wireTriple(viem, escrow, {
    name: "EscrowAdapter", current,
    desired: [args.intexAuctionContract, args.compactContract, args.paymentToken],
    labels: ["auction", "compact", "paymentToken"],
  });
}

function targetAddresses(args: TargetBridgeWireArgs) {
  const auction = (args.intexAuctionContract ?? "").trim();
  const intex = (args.intexContract ?? "").trim();
  const escrow = (args.escrowContract ?? "").trim();
  const empty: string[] = [];
  if (!auction) empty.push("--auction-contract");
  if (!intex) empty.push("--intex-contract");
  if (!escrow) empty.push("--escrow-contract");
  if (empty.length > 0) {
    throw new Error(
      `TargetRouter wire requires non-empty addresses. Missing: ${empty.join(", ")}. ` +
        `The deploy workflow reads them from dist/addresses/<network>.json - ensure the deploy step captured IntexAuction, IntexNFT1155, EscrowAdapter.`,
    );
  }
  return { auction, intex, escrow };
}

export async function targetBridgeWireAction(args: TargetBridgeWireArgs, hre: unknown) {
  const { auction, intex, escrow } = targetAddresses(args);
  const viem = await getViemForWire(hre);
  console.log(`Wiring TargetRouter...`);
  console.log(`  Bridge: ${args.bridgeContract}`);
  console.log(`  Auction: ${auction}`);
  console.log(`  Intex: ${intex}`);
  console.log(`  Escrow: ${escrow}`);

  const bridge = await viem.getContractAt("TargetRouter", args.bridgeContract as Address) as WiredContract;
  const current = await Promise.all([
    bridge.read.auction(), bridge.read.intex(), bridge.read.escrowAdapter(),
  ]);
  await wireTriple(viem, bridge, {
    name: "TargetRouter", current, desired: [auction, intex, escrow], labels: ["auction", "intex", "escrow"],
  });
}

export async function originBridgeWireAction(
  args: { bridgeContract: string; desisContract: string; intexFactoryContract: string },
  hre: unknown,
) {
  const viem = await getViemForWire(hre);
  console.log(`Wiring OriginRouter...`);
  console.log(`  Bridge: ${args.bridgeContract}`);
  console.log(`  Desis: ${args.desisContract}`);
  console.log(`  IntexFactory: ${args.intexFactoryContract}`);

  const bridge = await viem.getContractAt("OriginRouter", args.bridgeContract as Address) as WiredContract;
  const [currentDesis, currentIntexFactory] = await Promise.all([
    bridge.read.desis(), bridge.read.intexFactory(),
  ]);
  const desisMatch = currentDesis.toLowerCase() === args.desisContract.toLowerCase();
  const intexFactoryMatch = currentIntexFactory.toLowerCase() === args.intexFactoryContract.toLowerCase();
  if (desisMatch && intexFactoryMatch) {
    console.log(`[OK] OriginRouter already wired to this Desis + IntexFactory`);
    return;
  }
  if (currentDesis !== ZERO) {
    console.log(`[sync] Rewiring OriginRouter`);
    if (!desisMatch) console.log(`   desis: ${currentDesis} -> ${args.desisContract}`);
    if (!intexFactoryMatch) console.log(`   intexFactory: ${currentIntexFactory} -> ${args.intexFactoryContract}`);
  }
  const txHash = await sendAndWait(viem, () => bridge.write.wire([
    args.desisContract as Address, args.intexFactoryContract as Address,
  ]));
  console.log(`[OK] OriginRouter wired. Tx: ${txHash}`);
}
