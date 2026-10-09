import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { privateKeyToAccount } from "viem/accounts";
import { createCtx } from "../chain.js";
import { registerTools } from "../tools/index.js";
import { VERSION } from "../version.js";
import { FakeChain } from "./fake-chain.js";

export const OUTBE_RPC = "https://rpc.testnet.outbe.net";
export const BSC_RPC = "https://bsc-testnet-rpc.publicnode.com";
export const SIGNER_KEY = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
export const SIGNER = privateKeyToAccount(SIGNER_KEY).address;

export interface ToolCall {
  text: string;
  isError: boolean;
}

export interface Harness {
  chain: FakeChain;
  client: Client;
  call(name: string, args?: Record<string, unknown>): Promise<ToolCall>;
  close(): Promise<void>;
}

/** The real MCP server over an in-memory transport, dialing a fake chain. */
export async function startHarness(
  prepare: (chain: FakeChain) => void,
  { outbeChainId = 54_322_345, env = {} }: { outbeChainId?: number; env?: Record<string, string> } = {},
): Promise<Harness> {
  const chain = new FakeChain({ [OUTBE_RPC]: outbeChainId, [BSC_RPC]: 97, ...extraRpcs(env) }, SIGNER);
  prepare(chain);
  const restoreFetch = chain.install();
  const environment = { ...process.env };
  for (const name of Object.keys(process.env).filter((n) => n.startsWith("OUTBE_"))) delete process.env[name];
  process.env.OUTBE_PRIVATE_KEY = SIGNER_KEY;
  Object.assign(process.env, env);
  const ctx = await createCtx(OUTBE_RPC, SIGNER_KEY);
  const server = new McpServer({ name: "outbe-mcp", version: VERSION });
  registerTools(server, ctx);
  const [clientSide, serverSide] = InMemoryTransport.createLinkedPair();
  await server.connect(serverSide);
  const client = new Client({ name: "golden", version: "0" });
  await client.connect(clientSide);
  return {
    chain,
    client,
    async call(name, args = {}) {
      const result = await client.callTool({ name, arguments: args });
      const content = result.content as { type: string; text?: string }[];
      return { text: content.map((c) => c.text ?? "").join("\n"), isError: result.isError === true };
    },
    async close() {
      await client.close();
      await server.close();
      restoreFetch();
      process.env = environment;
    },
  };
}

/** Every `OUTBE_RPC_<chainId>` URL in `env`, served as that chain. */
function extraRpcs(env: Record<string, string>): Record<string, number> {
  return Object.fromEntries(
    Object.entries(env).flatMap(([name, url]) => {
      const id = /^OUTBE_RPC_(\d+)$/.exec(name)?.[1];
      return id ? [[url, Number(id)]] : [];
    }),
  );
}
