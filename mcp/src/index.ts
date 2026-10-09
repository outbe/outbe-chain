import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { createCtx } from "./chain.js";
import { loadConfig } from "./config.js";
import { registerTools } from "./tools/index.js";
import { VERSION } from "./version.js";

async function main(): Promise<void> {
  const { rpcUrl, privateKey } = loadConfig(process.argv.slice(2));

  const ctx = await createCtx(rpcUrl, privateKey);
  // stderr only - stdout is the MCP stdio channel.
  console.error(
    `[outbe-mcp] rpc=${rpcUrl} chainId=${ctx.chain.id} signer=${ctx.account?.address ?? "(read-only)"}`,
  );

  const server = new McpServer({ name: "outbe-mcp", version: VERSION });
  registerTools(server, ctx);

  await server.connect(new StdioServerTransport());
}

main().catch((e) => {
  console.error("[outbe-mcp] fatal:", e instanceof Error ? e.message : e);
  process.exit(1);
});
