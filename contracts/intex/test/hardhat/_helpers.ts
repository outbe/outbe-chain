// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Viem = any;

export async function deployIntexNFT1155(viem: Viem, args: readonly unknown[]) {
  return viem.deployContract("IntexNFT1155", args);
}
