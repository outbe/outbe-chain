import { ethers, Wallet } from "ethers";
import { SmartAccountFactory__factory, ExecutionDelayPolicy__factory, WithdrawalLimitPolicy__factory, IERC20__factory, IVaultRouter__factory, IEntryPoint__factory } from "./contracts/index.js";
import { ownerPermissionId, ccaPermissionId, permissionNonceKey, encodePermissionSignature } from "./utils.js";
import { singleExecution } from "./owner-execution.js";

export function requireIssuanceBalance(balance: bigint, principal: bigint): void {
  if (principal <= 0n || balance < principal) throw new Error("Smart account must already hold the requested stablecoin principal");
}
export function requirePermission(info: { hook: string; signer: string; policies: readonly string[] }, hook: string, signer: string, policy: string): void {
  const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
  if (!same(info.hook, hook) || !same(info.signer, signer) || info.policies.length !== 1 || !same(info.policies[0], policy)) {
    throw new Error("Required owner/CCA permission changed or revoked");
  }
}
export async function checkIssuance(wallet: Wallet, factoryAddress: string, account: string, owner: string,
  tokenAddress: string, principal: bigint, routerAddress: string, reservationId: string, entryPointAddress: string) {
  const provider = wallet.provider!;
  const block = await provider.getBlock("latest");
  if (!block || await provider.getCode(account) === "0x") throw new Error("Smart account is not deployed");
  const factory = SmartAccountFactory__factory.connect(factoryAddress, provider);
  const [delayAddress, limitAddress, signerAddress, kernelFactoryAddress] = await Promise.all([
    factory.executionDelayPolicy(), factory.withdrawalLimitPolicy(), factory.ecdsaSigner(), factory.kernelFactory(),
  ]);
  const kernel = new ethers.Contract(account, [
    "function root() view returns (bytes21)",
    "function validationInfo(bytes21) view returns (tuple(uint32 nonce,address hook,address signer,address[] policies))",
    "function isModuleInstalled(uint256,address,bytes) view returns (bool)",
  ], provider);
  const kernelFactory = new ethers.Contract(kernelFactoryAddress, ["function UUPS() view returns (address)"], provider);
  const implementation = await provider.getStorage(account, "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc");
  const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();
  if (!same(ethers.dataSlice(implementation, 12), await kernelFactory.UUPS())) throw new Error("Account implementation changed");
  const ownerId = ownerPermissionId();
  const ccaId = ccaPermissionId(tokenAddress);
  const vid = (id: string) => ethers.concat(["0x02", id, "0x" + "00".repeat(16)]);
  const [root, ownerInfo, ccaInfo] = await Promise.all([kernel.root(), kernel.validationInfo(vid(ownerId)), kernel.validationInfo(vid(ccaId))]);
  if (!same(root, vid(ownerId))) throw new Error("Owner root changed");
  requirePermission(ownerInfo, delayAddress, signerAddress, delayAddress);
  requirePermission(ccaInfo, "0x0000000000000000000000000000000000000001", signerAddress, limitAddress);
  const signer = new ethers.Contract(signerAddress, ["function signer(bytes32,address) view returns (address)"], provider);
  const padded = (id: string) => ethers.zeroPadBytes(id, 32);
  if (!same(await signer.signer(padded(ownerId), account), owner) || !same(await signer.signer(padded(ccaId), account), wallet.address)) throw new Error("Permission signer mismatch");
  const delay = ExecutionDelayPolicy__factory.connect(delayAddress, provider);
  const limit = WithdrawalLimitPolicy__factory.connect(limitAddress, provider);
  const cfg = await limit.configs(padded(ccaId), account);
  if (!same(cfg.token, tokenAddress) || cfg.amountLimit !== 1000_000000n || cfg.interval !== 86400n
    || await limit.status(padded(ccaId), account) !== 1n) throw new Error("CCA token restriction or daily cap mismatch");
  if (await delay.EXECUTION_DELAY() !== 300n || !await delay.hooked(account)
    || !same(await delay.permission(account), padded(ownerId))
    || !await kernel.isModuleInstalled(4, delayAddress, "0x")) throw new Error("Five-minute delay policy is inactive");
  // Treat pending arbitrary calls as potential configuration changes. Ordinary token transfers are safe to classify.
  for (const event of await delay.queryFilter(delay.filters.Scheduled(account), 0, block.number)) {
    if (await delay.readyAt(account, event.args.operationId) === 0n) continue;
    if (!same(await delay.operationId(account, event.args.execution), event.args.operationId)) continue;
    const data = ethers.getBytes(event.args.execution);
    const isTransfer = data.length === 228 && same(ethers.hexlify(data.slice(100, 120)), tokenAddress)
      && ethers.hexlify(data.slice(152, 156)) === "0xa9059cbb"
      && same(singleExecution(tokenAddress, ethers.hexlify(data.slice(152, 220))), event.args.execution);
    if (!isTransfer) throw new Error(`Pending owner execution may change account security: ${event.args.operationId}`);
  }
  const token = IERC20__factory.connect(tokenAddress, provider);
  requireIssuanceBalance(await token.balanceOf(account), principal);
  const reservation = await IVaultRouter__factory.connect(routerAddress, provider).reservationOf(reservationId);
  if (!same(reservation.cca, wallet.address) || !same(reservation.smartAccount, account) || !same(reservation.asset, tokenAddress)
    || reservation.amount < principal || BigInt(block.timestamp) > reservation.expiresAt) throw new Error("Reservation is missing, mismatched, insufficient, or expired");
  // Validate a signed zero-amount transfer to verify live selector access as well as installed modules.
  const ep = IEntryPoint__factory.connect(entryPointAddress, provider);
  const op = { sender: account, nonce: await ep.getNonce(account, permissionNonceKey(ccaId)), initCode: "0x",
    callData: singleExecution(tokenAddress, token.interface.encodeFunctionData("transfer", [wallet.address, 0n])),
    accountGasLimits: ethers.solidityPacked(["uint128", "uint128"], [2_000_000n, 2_000_000n]), preVerificationGas: 1_000_000n,
    gasFees: ethers.solidityPacked(["uint128", "uint128"], [1n, 1n]), paymasterAndData: "0x", signature: "0x" };
  const hash = await ep.getUserOpHash(op);
  op.signature = encodePermissionSignature(await wallet.signMessage(ethers.getBytes(hash)));
  const validation = new ethers.Interface(["function validateUserOp((address sender,uint256 nonce,bytes initCode,bytes callData,bytes32 accountGasLimits,uint256 preVerificationGas,bytes32 gasFees,bytes paymasterAndData,bytes signature),bytes32,uint256) returns (uint256)"]);
  const result = await provider.call({ from: entryPointAddress, to: account, data: validation.encodeFunctionData("validateUserOp", [op, hash, 0n]) });
  const [value] = validation.decodeFunctionResult("validateUserOp", result);
  if ((value & ((1n << 160n) - 1n)) !== 0n) throw new Error("CCA permission validation failed");
}
