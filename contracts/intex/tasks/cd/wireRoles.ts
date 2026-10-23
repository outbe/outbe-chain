import { getViemForWire, grantRoleIfMissing, type Address, type RoleContract } from "./wireClient.js";

interface FactoryRoleArgs {
  settlementContract: string;
  intexContract: string;
}

async function grantIntexRole(
  args: { intexContract: string; recipient: string },
  hre: unknown,
  config: { role: string; recipientLabel: string; heading: string },
) {
  const viem = await getViemForWire(hre);
  console.log(config.heading);
  console.log(`  ${config.recipientLabel}: ${args.recipient}`);
  console.log(`  IntexNFT1155: ${args.intexContract}`);

  const intex = await viem.getContractAt("IntexNFT1155", args.intexContract as Address) as RoleContract;
  const role = await intex.read[config.role]();
  await grantRoleIfMissing(viem, intex, role, args.recipient as Address, {
    existing: `[OK] IntexNFT1155: ${config.recipientLabel} already has ${config.role}`,
    granted: tx => `[OK] IntexNFT1155: ${config.role} granted to ${config.recipientLabel}. Tx: ${tx}`,
  });
}

export async function settlementGrantRolesAction(args: FactoryRoleArgs, hre: unknown) {
  await grantIntexRole({ intexContract: args.intexContract, recipient: args.settlementContract }, hre, {
    role: "SETTLEMENT_ROLE", recipientLabel: "IntexFactory", heading: "Granting roles for IntexFactory...",
  });
}

export async function promisWireAction(args: FactoryRoleArgs, hre: unknown) {
  await grantIntexRole({ intexContract: args.intexContract, recipient: args.settlementContract }, hre, {
    role: "PROMIS_ROLE", recipientLabel: "IntexFactory", heading: "Granting PROMIS_ROLE on IntexNFT1155...",
  });
}

export async function gemWireAction(args: { gemFactory: string; intexContract: string }, hre: unknown) {
  await grantIntexRole({ intexContract: args.intexContract, recipient: args.gemFactory }, hre, {
    role: "GEM_ROLE", recipientLabel: "GemFactory", heading: "Granting GEM_ROLE on IntexNFT1155...",
  });
}

export async function grantRelayerRoleAction(args: { token: string; adapter: string; contract: string }, hre: unknown) {
  const viem = await getViemForWire(hre);
  const contractName = args.contract || "IntexNFT1155";
  console.log(`Granting RELAYER_ROLE on ${contractName} @ ${args.token} to ${args.adapter}...`);

  const token = await viem.getContractAt(contractName, args.token as Address) as RoleContract;
  const role = await token.read.RELAYER_ROLE();
  await grantRoleIfMissing(viem, token, role, args.adapter as Address, {
    existing: "[OK] RELAYER_ROLE already granted",
    granted: tx => `[OK] RELAYER_ROLE granted. Tx: ${tx}`,
  });
}
