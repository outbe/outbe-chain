import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { createCtx } from "./chain.js";
import { registerIntentTools } from "./tools/intent.js";
import { registerRpcTools } from "./tools/rpc.js";
import { registerSignTools } from "./tools/sign.js";
import { registerViewTools } from "./tools/view.js";

const DEFAULT_RPC = "https://rpc.testnet.outbe.net";

function parseRpc(argv: string[]): string {
  const i = argv.indexOf("--rpc");
  if (i >= 0 && argv[i + 1]) return argv[i + 1];
  return process.env.OUTBE_RPC ?? DEFAULT_RPC;
}

async function main(): Promise<void> {
  const rpcUrl = parseRpc(process.argv.slice(2));
  const privateKey = process.env.OUTBE_PRIVATE_KEY;

  const ctx = await createCtx(rpcUrl, privateKey);
  // stderr only — stdout is the MCP stdio channel.
  console.error(
    `[outbe-mcp] rpc=${rpcUrl} chainId=${ctx.chain.id} signer=${ctx.account?.address ?? "(read-only)"}`,
  );

  const server = new McpServer({ name: "outbe-mcp", version: "0.1.0" });
  registerRpcTools(server, ctx);
  registerViewTools(server, ctx);
  registerSignTools(server, ctx);
  registerIntentTools(server, ctx);

  await server.connect(new StdioServerTransport());
}

main().catch((e) => {
  console.error("[outbe-mcp] fatal:", e instanceof Error ? e.message : e);
  process.exit(1);
});
