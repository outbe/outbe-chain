import type { Hex } from "viem";
import { PRECOMPILE_ABI } from "../abi.js";
import type { Ctx } from "../chain.js";
import type { Network } from "../net/resolver.js";
import { CONTRACTS } from "../registry.js";
import { accountKeys, forgetKeys } from "./keys.js";
import { type Ledger, chainIdWord, modifyMac } from "./mac.js";

export interface Write {
  ledger: Ledger;
  op: number;
  amount: bigint;
}

export interface Authorization {
  mac: Hex;
  opNonce: bigint;
}

const LEDGERS = {
  Gratis: { address: CONTRACTS.gratis.address, abi: PRECOMPILE_ABI.IGratis },
  Promis: { address: CONTRACTS.promis.address, abi: PRECOMPILE_ABI.IPromis },
} as const;

const tails = new Map<string, Promise<void>>();

/** Serialises writes per account and ledger: two writes reading one opNonce would revert the second. */
async function lock(key: string): Promise<() => void> {
  const previous = tails.get(key) ?? Promise.resolve();
  let release!: () => void;
  const current = new Promise<void>((resolve) => {
    release = resolve;
  });
  const tail = previous.then(() => current);
  tails.set(key, tail);
  await previous;
  return () => {
    release();
    if (tails.get(key) === tail) tails.delete(key);
  };
}

async function opNonceOf(network: Network, ledger: Ledger, account: Hex): Promise<bigint> {
  const { address, abi } = LEDGERS[ledger];
  return await network.client.readContract({ address, abi, functionName: "opNonceOf", args: [account] });
}

function signer(ctx: Ctx) {
  if (!ctx.account) throw new Error("signing requires a key - set OUTBE_PRIVATE_KEY in the MCP server env");
  return ctx.account;
}

async function authorizeAll(ctx: Ctx, network: Network, writes: readonly Write[]): Promise<Authorization[]> {
  const account = signer(ctx);
  return await Promise.all(
    writes.map(async ({ ledger, op, amount }) => {
      const keys = await accountKeys(network, ledger, account);
      const opNonce = await opNonceOf(network, ledger, account.address);
      const chainId = chainIdWord(network.chainId);
      return { mac: modifyMac(keys.modify, { ledger, account: account.address, op, amount, opNonce, chainId }), opNonce };
    }),
  );
}

/**
 * Authorizes `writes` for the signer on `network` and runs `send` with them while holding the
 * signer's lock on each ledger, so `send` must return only once its receipt is in. When the chain
 * rejects the MAC, the cached keys are dropped and the write is authorized and sent once more.
 */
export async function withAuthorization<T>(
  ctx: Ctx,
  network: Network,
  writes: readonly Write[],
  send: (authorizations: Authorization[]) => Promise<T>,
): Promise<T> {
  const account = signer(ctx);
  const ledgers = [...new Set(writes.map((w) => w.ledger))].sort();
  const releases = [];
  for (const ledger of ledgers) releases.push(await lock(`${network.chainId}:${ledger}:${account.address}`));
  try {
    try {
      return await send(await authorizeAll(ctx, network, writes));
    } catch (error) {
      if (!/invalid modify authorization/i.test(String((error as Error).message))) throw error;
      for (const ledger of ledgers) forgetKeys(network, ledger, account.address);
      return await send(await authorizeAll(ctx, network, writes));
    }
  } finally {
    for (const release of releases.reverse()) release();
  }
}
