import { encodeFunctionData } from "viem";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Viem = any;

async function deployUupsProxy(viem: Viem, contractName: string, initArgs: readonly unknown[]) {
  const impl = await viem.deployContract(contractName);
  const initData = encodeFunctionData({ abi: impl.abi, functionName: "initialize", args: initArgs });
  const proxy = await viem.deployContract("ERC1967Proxy", [impl.address, initData]);
  return viem.getContractAt(contractName, proxy.address);
}

export async function deployIntexNFT1155(viem: Viem, args: readonly unknown[]) {
  return deployUupsProxy(viem, "IntexNFT1155", args);
}

export async function deployIntexAuction(viem: Viem, args: readonly unknown[]) {
  return deployUupsProxy(viem, "IntexAuction", args);
}

export async function deployEscrowAdapter(viem: Viem, args: readonly unknown[]) {
  return deployUupsProxy(viem, "EscrowAdapter", args);
}
