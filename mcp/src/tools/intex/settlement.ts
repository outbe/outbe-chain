import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { PRECOMPILE_ABI } from "../../abi.js";
import { encodeFunctionData, formatUnits, getAddress } from "viem";
import { z } from "zod";
import { OUTBE_NETWORK } from "../../net/chains.js";
import { requireAccount } from "../../net/tx.js";
import { networkName, waitFlag } from "../schemas.js";
import { ensureAllowance } from "../../net/erc20.js";
import { handler, ok } from "../util.js";
import { ERC20_ABI, FACTORY_ABI, INTEX_ABI } from "../../intex/registry.js";
import { grindNonce } from "../../intex/pow.js";
import { addr, exerciseCount, quoteSettlement, settlementTokens } from "../../intex/reads.js";
import { withAuthorization } from "../../confidential/authorize.js";
import { OP } from "../../confidential/mac.js";
import { openPromis, signerAmounts } from "../../confidential/balances.js";
import { accountArg, seriesArg, unitsArg } from "./args.js";
import type { IntexDeps } from "./deps.js";

/** Settlement and Promis on outbe. */
export function registerSettlementTools(server: McpServer, deps: IntexDeps): void {
  const { ctx, resolveNetwork, whoever, submit } = deps;
  server.tool(
    "intex_settle",
    "Settlement step 1: pay the strike and turn Issued Intexes into Settled (Promis is mined later via " +
      "intex_promis_mine). Pays in `token`, one of the tokens intex_settlement_tokens lists: the tool " +
      "quotes the units, approves IntexFactory for that cost if the allowance is short, and settles at " +
      "the quoted snapshot. " +
      "Defaults to your own wallet; pass owner to pay for someone else's position. " +
      "Allowed while the series is Issued (voluntary) or once it is Called " +
      "(forced, within the call period). The " +
      "Settled token (soulbound) stays with the owner whoever pays, and only the owner can mine its Promis. " +
      "Settlement only ever happens on outbe: a position sitting on a target chain has to be brought over with " +
      "intex_bridge_send first, and that has to land before the series settlementDeadline. Requires " +
      "OUTBE_PRIVATE_KEY.",
    {
      series: seriesArg,
      units: unitsArg,
      token: z.string().describe("settlement token address, one listed by intex_settlement_tokens"),
      owner: z.string().optional().describe("owner of the units (default: the configured signer)"),
      network: networkName.optional(),
      wait: waitFlag,
    },
    handler(async ({ series, units, token, owner, network, wait }) => {
      const n = await resolveNetwork(network ?? OUTBE_NETWORK);
      const account = requireAccount(ctx);
      const holder = owner ? getAddress(owner) : account.address;
      const asset = getAddress(token);
      const quantity = BigInt(units);
      const { settlementCurrency, paymentMinor, snapshotId } = await quoteSettlement(n, series, asset, quantity);

      const factory = addr(n, "factory");
      const approval =
        paymentMinor > 0n ? await ensureAllowance(ctx, n, { token: asset, spender: factory, amount: paymentMinor }) : null;
      const autoApprove = approval ? { txHash: approval, amount: paymentMinor.toString() } : null;
      const data = encodeFunctionData({
        abi: FACTORY_ABI,
        functionName: "settleIntex",
        args: [series, holder, quantity, asset, snapshotId],
      });
      const receipt = await submit(n, factory, data, 0n, wait);
      return ok({
        network: n.name,
        series,
        owner: holder,
        units,
        self: holder === account.address,
        token: asset,
        settlementCurrency,
        paymentMinor: paymentMinor.toString(),
        snapshotId: snapshotId.toString(),
        autoApprove,
        ...receipt,
      });
    }),
  );

  server.tool(
    "intex_settlement_tokens",
    "Tokens you can settle a series with and what settling `units` of it costs in each; " +
      "intex_settle pays that cost in the token you pick. The chain floors the whole " +
      "operation once, so quote the units you will actually settle. An issuance-currency cost holds " +
      "only until the next whole UTC hour (its `snapshotId` changes then), so settle within that hour " +
      "or quote again.",
    { series: seriesArg, units: z.number().int().positive().optional(), network: networkName.optional() },
    handler(async ({ series, units, network }) => {
      const n = await resolveNetwork(network ?? OUTBE_NETWORK);
      const quoted = BigInt(units ?? 1);
      const tokens = await settlementTokens(n, series);
      const priced = await Promise.all(
        tokens.map(async (token) => {
          const [decimals, symbol] = await Promise.all([
            n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" }),
            n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "symbol" }),
          ]);
          const base = { token, symbol: symbol as string, decimals: Number(decimals) };
          // A refused issuance-currency quote is this token's answer, not the list's.
          try {
            const { settlementCurrency, paymentMinor, snapshotId } = await quoteSettlement(
              n,
              series,
              token,
              quoted,
            );
            return {
              ...base,
              settlementCurrency,
              snapshotId: snapshotId.toString(),
              cost: { raw: paymentMinor.toString(), value: formatUnits(paymentMinor, Number(decimals)) },
            };
          } catch (error) {
            return { ...base, unavailable: (error as Error).message };
          }
        }),
      );
      return ok({ network: n.name, series, units: quoted.toString(), tokens: priced });
    }),
  );

}

/** Promis mined from Settled Intex, and the signer's Promis balance. */
export function registerPromisTools(server: McpServer, deps: IntexDeps): void {
  const { ctx, resolveNetwork, whoever, submit } = deps;
  server.tool(
    "intex_promis_mine",
    "Settlement step 2: burn your Settled Intexes and mine Promis to your own wallet (run intex_settle " +
      "first). The proof of work and the Promis modify authorization are computed here; you give only " +
      "series and units. Waits for the receipt. Requires OUTBE_PRIVATE_KEY.",
    { series: seriesArg, units: unitsArg, network: networkName.optional() },
    handler(async ({ series, units, network }) => {
      const n = await resolveNetwork(network ?? OUTBE_NETWORK);
      const owner = requireAccount(ctx).address;
      const amount = BigInt(units);
      const sd = await n.client.readContract({
        address: addr(n, "intex"),
        abi: INTEX_ABI,
        functionName: "getIntexSeries",
        args: [series],
      });
      const promisMinor = sd.promisLoadMinor * amount;
      const difficulty = await n.client.readContract({
        address: addr(n, "factory"),
        abi: FACTORY_ABI,
        functionName: "powDifficulty",
      });
      // The mining sequence moves with every Promis mint, so it is read under the same lock.
      const { pow, seq, sent } = await withAuthorization(
        ctx,
        n,
        [{ ledger: "Promis", op: OP.Promis.Mint, amount: promisMinor }],
        async ([auth]) => {
          const seq = await exerciseCount(n, series, owner);
          const pow = grindNonce(owner, promisMinor, series, seq, difficulty);
          const data = encodeFunctionData({
            abi: FACTORY_ABI,
            functionName: "minePromis",
            args: [series, owner, amount, pow.nonce, auth.mac, auth.opNonce],
          });
          return { pow, seq, sent: await submit(n, addr(n, "factory"), data, 0n) };
        },
      );
      return ok({
        network: n.name,
        series,
        owner,
        units: amount.toString(),
        promisMinor: promisMinor.toString(),
        pow: { nonce: pow.nonce.toString(), seq, difficulty },
        ...sent,
      });
    }),
  );

  server.tool(
    "intex_promis_balance",
    "Encrypted Promis balance for an address on outbe. For the signer's own account it is also shown " +
      "decrypted with its Promis view key.",
    { account: accountArg, network: networkName.optional() },
    handler(async ({ account, network }) => {
      const n = await resolveNetwork(network ?? OUTBE_NETWORK);
      const who = whoever(account);
      const balance = await n.client.readContract({
        address: addr(n, "promis"),
        abi: PRECOMPILE_ABI.IPromis,
        functionName: "balanceOf",
        args: [who],
      });
      const decrypted = await signerAmounts(ctx, n, "Promis", who, (key) => ({ balance: openPromis(key, who, balance) }));
      return ok({ network: n.name, account: who, balance, ...(decrypted && { decrypted }) });
    }),
  );
}
