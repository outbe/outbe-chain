import { getViemForWire, grantRoleIfMissing, type Address, type RoleContract, type WireViem } from "./wireClient.js";

interface SystemGrantRolesArgs {
  bridgeContract: string;
  intexContract: string;
  systemAddress: string;
  desisContract: string;
}

function requireAddress(value: string, label: string, suffix = "") {
  if (!value || value === "null") {
    throw new Error(`${label} is required${suffix}`);
  }
}

function grantCallerRole(viem: WireViem, contract: RoleContract, contractName: string) {
  return async (label: string, role: Address, addr: Address) => {
    await grantRoleIfMissing(viem, contract, role, addr, {
      existing: `[OK] ${contractName}: ${addr} already has ${label}`,
      granted: tx => `[OK] ${contractName}: ${label} -> ${addr}. Tx: ${tx}`,
    });
  };
}

export async function systemGrantRolesAction(args: SystemGrantRolesArgs, hre: unknown) {
  const viem = await getViemForWire(hre);
  requireAddress(args.bridgeContract, "bridgeContract (OriginRouter)");
  requireAddress(args.intexContract, "intexContract (IntexNFT1155)");
  requireAddress(args.systemAddress, "systemAddress (OUTBE_SYSTEM_TX_ADDRESS)");
  requireAddress(args.desisContract, "desisContract (Desis precompile)");
  const systemAddress = args.systemAddress as Address;
  const desisAddress = args.desisContract as Address;

  console.log(`Granting precompile-caller roles...`);
  console.log(`  SystemCaller:    ${systemAddress}`);
  console.log(`  Desis:           ${desisAddress}`);
  console.log(`  OriginRouter: ${args.bridgeContract}`);
  console.log(`  IntexNFT1155:    ${args.intexContract}`);

  const router = await viem.getContractAt("OriginRouter", args.bridgeContract as Address) as RoleContract;
  const intex = await viem.getContractAt("IntexNFT1155", args.intexContract as Address) as RoleContract;
  const grantOnRouter = grantCallerRole(viem, router, "OriginRouter");
  const grantOnIntex = grantCallerRole(viem, intex, "IntexNFT1155");

  const desisRole = await router.read.DESIS_ROLE();
  const intexFactoryRole = await router.read.INTEX_FACTORY_ROLE();
  const relayerRole = await intex.read.RELAYER_ROLE();
  await grantOnRouter("DESIS_ROLE", desisRole, systemAddress);
  await grantOnRouter("INTEX_FACTORY_ROLE", intexFactoryRole, systemAddress);
  await grantOnIntex("RELAYER_ROLE", relayerRole, systemAddress);
  await grantOnRouter("INTEX_FACTORY_ROLE", intexFactoryRole, desisAddress);
  await grantOnIntex("RELAYER_ROLE", relayerRole, desisAddress);
}

export async function intexFactoryAssertRelayerRoleAction(
  args: { intexContract: string; desisContract: string; systemAddress: string },
  hre: unknown,
) {
  const viem = await getViemForWire(hre);
  const suffix = " to assert RELAYER_ROLE";
  requireAddress(args.intexContract, "intexContract (IntexNFT1155)", suffix);
  requireAddress(args.desisContract, "desisContract (Desis precompile)", suffix);
  requireAddress(args.systemAddress, "systemAddress (OUTBE_SYSTEM_TX_ADDRESS)", suffix);
  const desisAddress = args.desisContract as Address;
  const systemAddress = args.systemAddress as Address;

  console.log(`Asserting RELAYER_ROLE on IntexNFT1155 for the issuance + mark callers...`);
  console.log(`  IntexNFT1155: ${args.intexContract}`);
  console.log(`  Desis:        ${desisAddress} (createSeries)`);
  console.log(`  SystemCaller: ${systemAddress} (markCalled)`);

  const intex = await viem.getContractAt("IntexNFT1155", args.intexContract as Address) as RoleContract;
  const role = await intex.read.RELAYER_ROLE();
  for (const addr of [desisAddress, systemAddress]) {
    if (!(await intex.read.hasRole([role, addr]))) {
      throw new Error(
        `${addr} does NOT hold RELAYER_ROLE on IntexNFT1155 ${args.intexContract}. ` +
          `Issuance (createSeries) or call (markCalled) will revert. ` +
          `Grant it first: outbe-system-grant-roles --bridge-contract <router> --intex-contract <intex> --system-address <system> --desis-contract <desis>.`,
      );
    }
  }
  console.log(`[OK] Desis precompile and system caller hold RELAYER_ROLE on IntexNFT1155`);
}
