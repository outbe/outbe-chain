import { ethers, Wallet } from "ethers";
import { IEntryPoint__factory } from "./contracts/index.js";
import { ownerPermissionId, permissionNonceKey, encodePermissionSignature, ENTRYPOINT_MIN_DEPOSIT, ENTRYPOINT_TOPUP } from "./utils.js";

/** Submit an owner action and reject a mined UserOperation execution failure. */
export async function ownerCall(wallet: Wallet, entryPointAddress: string, account: string, target: string, data: string): Promise<void> {
  const ep = IEntryPoint__factory.connect(entryPointAddress, wallet);
  if (await ep.balanceOf(account) < ENTRYPOINT_MIN_DEPOSIT) {
    await (await ep.depositTo(account, { value: ENTRYPOINT_TOPUP })).wait();
  }
  const kernel = new ethers.Interface(["function execute(bytes32 mode, bytes executionData)"]);
  const callData = ethers.concat(["0x8dd7712f", kernel.encodeFunctionData("execute", [ethers.ZeroHash,
    ethers.solidityPacked(["address", "uint256", "bytes"], [target, 0n, data])])]);
  const op = {
    sender: account, nonce: await ep.getNonce(account, permissionNonceKey(ownerPermissionId())),
    initCode: "0x", callData,
    accountGasLimits: ethers.solidityPacked(["uint128", "uint128"], [3_000_000n, 3_000_000n]),
    preVerificationGas: 1_000_000n,
    gasFees: ethers.solidityPacked(["uint128", "uint128"], [1n, 1n]),
    paymasterAndData: "0x", signature: "0x",
  };
  const hash = await ep.getUserOpHash(op);
  op.signature = encodePermissionSignature(await wallet.signMessage(ethers.getBytes(hash)));
  const receipt = await (await ep.handleOps([op], wallet.address)).wait();
  const event = receipt?.logs.filter(log => log.address.toLowerCase() === entryPointAddress.toLowerCase())
    .map(log => { try { return ep.interface.parseLog(log); } catch { return null; } })
    .find(log => log?.name === "UserOperationEvent" && log.args[0] === hash);
  if (!event?.args[4]) throw new Error(`Account action failed: ${receipt?.hash}`);
}
