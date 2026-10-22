import { createPublicClient, createWalletClient, getContract, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { getEnvRpcAndPk, makeChain } from "../../scripts/shared/chains.js";
import { getNetworkName } from "../../scripts/shared/taskUtils.js";
import { loadAbi } from "../../scripts/shared/abi.js";

export type Address = `0x${string}`;

export async function getViemForWire(hre: unknown) {
  const networkName = getNetworkName(hre);
  const { rpc, pk } = getEnvRpcAndPk(networkName);
  if (!pk) throw new Error(`Private key required for ${networkName}`);
  const chain = makeChain(networkName, rpc);
  const account = privateKeyToAccount(pk as Address);
  const transport = http(rpc);
  const publicClient = createPublicClient({ chain, transport });
  const walletClient = createWalletClient({ account, chain, transport });
  return {
    getContractAt: async (name: string, address: Address): Promise<unknown> =>
      getContract({ address, abi: loadAbi(name), client: { public: publicClient, wallet: walletClient } }),
    getPublicClient: async () => publicClient,
  };
}

export type WireViem = Awaited<ReturnType<typeof getViemForWire>>;

export async function sendAndWait(viem: WireViem, writeFn: () => Promise<Address>): Promise<Address> {
  const hash = await writeFn();
  const publicClient = await viem.getPublicClient();
  await publicClient.waitForTransactionReceipt({ hash });
  return hash;
}

export type RoleContract = {
  read: Record<string, () => Promise<Address>> & {
    hasRole: (args: [Address, Address]) => Promise<boolean>;
  };
  write: {
    grantRole: (args: [Address, Address]) => Promise<Address>;
  };
};

export async function grantRoleIfMissing(
  viem: WireViem,
  contract: RoleContract,
  role: Address,
  recipient: Address,
  messages: { existing: string; granted: (hash: Address) => string },
) {
  if (await contract.read.hasRole([role, recipient])) {
    console.log(messages.existing);
  } else {
    const tx = await sendAndWait(viem, () => contract.write.grantRole([role, recipient]));
    console.log(messages.granted(tx));
  }
}
