import { ethers, Wallet, TransactionReceipt } from "ethers";
import { mkdirSync, readFileSync, existsSync, writeFileSync, renameSync } from "node:fs";
import { resolve } from "node:path";
import { IEntryPoint__factory, ExecutionDelayPolicy__factory } from "./contracts/index.js";
import { TICKETS_DIR } from "./ticket.js";
import { ownerPermissionId, permissionNonceKey, encodePermissionSignature, ENTRYPOINT_MIN_DEPOSIT, ENTRYPOINT_TOPUP } from "./utils.js";

export const kernelInterface = new ethers.Interface(["function execute(bytes32 mode, bytes execution)"]);
export function singleExecution(target: string, data: string): string {
  return kernelInterface.encodeFunctionData("execute", [ethers.ZeroHash, ethers.solidityPacked(["address", "uint256", "bytes"], [target, 0n, data])]);
}

/** Persist exact calldata before broadcasting, then resume against on-chain readiness. */
export async function delayedOwnerExecution(wallet: Wallet, account: string, entryPointAddress: string, policyAddress: string,
  label: string, execution: string): Promise<TransactionReceipt | null> {
  const provider = wallet.provider!;
  const chain = (await provider.getNetwork()).chainId;
  const policy = ExecutionDelayPolicy__factory.connect(policyAddress, provider);
  const ep = IEntryPoint__factory.connect(entryPointAddress, wallet);
  const id = await policy.operationId(account, execution);
  const path = resolve(TICKETS_DIR, `execution-${chain}-${account.toLowerCase()}-${label}.json`);
  type Request = { execution: string; operationId: string; completed?: boolean; scheduleTx?: string; executionTx?: string };
  let record: Request = { execution, operationId: id };
  if (existsSync(path)) {
    const previous = JSON.parse(readFileSync(path, "utf8")) as Request;
    if (!previous.completed) {
      if (previous.operationId !== id || previous.execution !== execution) {
        throw new Error(`Pending request differs from this execution. Resume with its original arguments or cancel it first: ${path}`);
      }
      record = previous;
    }
  }
  mkdirSync(TICKETS_DIR, { recursive: true, mode: 0o700 });
  const save = () => {
    writeFileSync(`${path}.tmp`, JSON.stringify(record, null, 2), { mode: 0o600 });
    renameSync(`${path}.tmp`, path);
  };
  save();
  const checkReceipt = (receipt: TransactionReceipt) => {
    const event = receipt.logs.filter(log => log.address.toLowerCase() === entryPointAddress.toLowerCase())
      .map(log => { try { return ep.interface.parseLog(log); } catch { return null; } })
      .find(log => log?.name === "UserOperationEvent" && log.args.sender.toLowerCase() === account.toLowerCase());
    if (!event?.args.success) throw new Error(`Owner operation failed; its request remains retryable: ${receipt.hash}`);
    return receipt;
  };
  // Recover broadcasts interrupted before their receipt was persisted; never send a duplicate.
  if (record.executionTx) {
    const receipt = await provider.getTransactionReceipt(record.executionTx);
    if (!receipt) throw new Error(`Execution transaction still pending: ${record.executionTx}`);
    if (receipt.status === 1) {
      try { checkReceipt(receipt); record.completed = true; save(); return receipt; }
      catch { /* Failed inner execution can be retried using the same request. */ }
    }
    delete record.executionTx;
    save();
  }
  const send = async (inner: string, phase: "scheduleTx" | "executionTx") => {
    if (await ep.balanceOf(account) < ENTRYPOINT_MIN_DEPOSIT) await (await ep.depositTo(account, { value: ENTRYPOINT_TOPUP })).wait();
    const op = {
      sender: account, nonce: await ep.getNonce(account, permissionNonceKey(ownerPermissionId())), initCode: "0x",
      callData: ethers.concat(["0x8dd7712f", inner]),
      accountGasLimits: ethers.solidityPacked(["uint128", "uint128"], [2_000_000n, 2_000_000n]),
      preVerificationGas: 1_000_000n, gasFees: ethers.solidityPacked(["uint128", "uint128"], [1n, 1n]),
      paymasterAndData: "0x", signature: "0x",
    };
    op.signature = encodePermissionSignature(await wallet.signMessage(ethers.getBytes(await ep.getUserOpHash(op))));
    const tx = await ep.handleOps([op], wallet.address);
    record[phase] = tx.hash;
    save();
    const receipt = await tx.wait();
    if (!receipt) throw new Error(`Receipt missing: ${tx.hash}`);
    return checkReceipt(receipt);
  };
  let ready = await policy.readyAt(account, id);
  if (ready === 0n) {
    if (record.scheduleTx) {
      const receipt = await provider.getTransactionReceipt(record.scheduleTx);
      if (!receipt) throw new Error(`Scheduling transaction still pending: ${record.scheduleTx}`);
      // A successfully scheduled request that disappeared was cancelled or executed elsewhere.
      if (receipt.status === 1) {
        checkReceipt(receipt);
        throw new Error(`Request was cancelled or consumed externally; inspect ${path} before starting a new request.`);
      }
    }
    await send(singleExecution(policyAddress, policy.interface.encodeFunctionData("schedule", [execution])), "scheduleTx");
    ready = await policy.readyAt(account, id);
  }
  const block = await provider.getBlock("latest");
  if (!block || BigInt(block.timestamp) < ready) {
    console.log(`Scheduled ${id}. Ready at ${ready}; rerun the same command to resume. Saved: ${path}`);
    return null;
  }
  const receipt = await send(execution, "executionTx");
  record.completed = true;
  save();
  return receipt;
}
