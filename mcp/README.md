# @outbe/mcp

Local **stdio MCP server** for outbe-chain. Lets Claude Code / Codex (or any MCP
client) read chain and precompile state and submit a curated set of signed
transactions — with **human-decoded output** (WorldwideDay → date, `*_minor` →
whole COEN, currency code → symbol, status enum → name, tokenURI JSON parsed).

Pure JS, no Python, no native build deps (`viem` + `@noble/*` + the official MCP SDK).

## Configure (Claude Code / Codex / Claude Desktop)

### Claude Code — without editing any config file

Use `claude mcp add`; it writes the config for you.

Local (before publish), pointing at the built file:

```bash
claude mcp add outbe \
  --scope user \
  -e OUTBE_PRIVATE_KEY=0x... \
  -- node /abs/path/outbe-chain/mcp/dist/index.js --rpc https://rpc.testnet.outbe.net
```

After publishing to npm:

```bash
claude mcp add outbe --scope user -e OUTBE_PRIVATE_KEY=0x... \
  -- npx -y @outbe/mcp --rpc https://rpc.testnet.outbe.net
```

- `--scope user` makes it available in every project; `project` writes a shared
  `.mcp.json` in the repo; `local` (default) is this project only.
- `-e KEY=VALUE` sets env vars; everything after `--` is the launch command.
- Omit `-e OUTBE_PRIVATE_KEY` for a read-only server.

Manage / verify:

```bash
claude mcp list          # connection status
claude mcp get outbe     # details
claude mcp remove outbe  # remove
```

In a session, `/mcp` lists connected servers and their tools.

### Manual config (any MCP client)

After publishing to npm:

```json
{
  "mcpServers": {
    "outbe": {
      "command": "npx",
      "args": ["-y", "@outbe/mcp", "--rpc", "https://rpc.testnet.outbe.net"],
      "env": { "OUTBE_PRIVATE_KEY": "0x…" }
    }
  }
}
```

Local (before publish), point at the built file:

```json
{
  "mcpServers": {
    "outbe": {
      "command": "node",
      "args": ["/abs/path/outbe-chain/mcp/dist/index.js", "--rpc", "https://rpc.testnet.outbe.net"],
      "env": { "OUTBE_PRIVATE_KEY": "0x…" }
    }
  }
}
```

- `--rpc` (or `OUTBE_RPC`) — node URL. Default `https://rpc.testnet.outbe.net`.
- `OUTBE_PRIVATE_KEY` — **optional**. View tools work without it; signing tools
  require it. The key is read from env only, never passed as a tool argument.
- Chain id is read from the node (`eth_chainId`); no fork assumptions.

## Build / dev

```bash
cd mcp
npm install
npm run build      # -> dist/index.js (bin: outbe-mcp)
npm run dev        # tsx, no build
npm run typecheck
```

Inspect interactively:

```bash
OUTBE_RPC=https://rpc.testnet.outbe.net \
  npx @modelcontextprotocol/inspector node dist/index.js
```

## Tools

**Chain RPC** — `chain_info`, `get_block`, `get_transaction`, `get_transaction_receipt`.

**Generic view** — `contract_call { contract, method, args[] }`: any view/pure method
on any precompile, decoded. `contract` is a registry name or a `0x` address.

**Convenience reads** (decoded) — `tribute_get`, `tributes_by_owner`, `tributes_by_day`,
`tribute_day_totals`, `nod_get`, `nods_by_owner`, `gem_get`, `gems_by_owner`,
`gratis_balance`, `promis_balance`,
`fidelity_index`, `agentreward_claimable`, `offering_days`, `worldwide_day`,
`oracle_pairs`, `oracle_rate`, `oracle_vwap`, `validators`, `validator`,
`staking_info`, `rewards_pending`.

**Signing (allowlist, need `OUTBE_PRIVATE_KEY`)** — `tribute_offer`, `staking_stake`,
`staking_unstake`, `staking_claim_unbonded`, `rewards_claim`, `agentreward_claim`,
`oracle_delegate_feeder`, `oracle_submit_vote`. Amounts are whole COEN strings (`"100"`,
`"1.5"`), scaled to 1e18 minor units internally. Transactions are EIP-1559 (type 2) with
an explicit gas limit (`tribute_offer` can't be `eth_estimateGas`-simulated because the
payload is decrypted inside the enclave during execution).

### `tribute_offer`

Reads the DKG-derived offer key from the TeeRegistry, auto-detects the OFFERING
WorldwideDay (or takes `worldwide_day`), encrypts the payload (X25519 ECDHE +
HKDF-SHA256, salt `[0x03;32]`, info `"tribute-factory-encryption"` +
ChaCha20Poly1305) — **byte-identical to the enclave decrypt path** — and sends
`offerTribute`. The token id is derived from `(caller, worldwide_day)`, so one
tribute per account per day.

## Intent (cross-chain orders)

Tools for the ERC-7683 `LayerZeroRouter` (cross-chain swap intents). Unlike the
precompile tools above, the router is a regular deployed contract used across
several networks. The supported networks live in the `NETWORKS` table in
`src/intent/registry.ts`

- `intent_open_order { origin, destination, input_token, output_token, amount_in, … }`
  — approves the input (or sends native value) and calls `open`; returns the
  deterministic `orderId`. Amounts are whole-token decimals (input decimals read
  on origin, output on destination). Needs `OUTBE_PRIVATE_KEY`.
- `intent_track_order { order_id, chain }` — deterministic lifecycle snapshot:
  derives a coarse `phase` (`OPENED → CLAIMED → FILLED → SETTLED`, plus
  `REFUNDED`/`EXPIRED`) with a `next` hint. No event scan; poll it (e.g. via
  `/loop`) to follow progress.
- `intent_refund_order { order_id, chain }` — refunds an expired, still-`OPENED`
  order to the sender; cross-chain refunds quote the LayerZero fee automatically,
  same-chain refunds are free. Needs `OUTBE_PRIVATE_KEY`.

**Example prompts:** 

- *"Open an intent order: swap 1 USDT on bsc-testnet for 20 COEN on
  outbe-testnet."* → `intent_open_order { origin: "bsc-testnet", destination:
  "outbe-testnet", input_token: "USDT", output_token: "COEN", amount_in: "1",
  amount_out: "20" }`
- *"Reverse it: 10 COEN on outbe-testnet → 0.1 USDT on bsc-testnet."* →
  `origin: "outbe-testnet", input_token: "COEN", amount_in: "10",
  destination: "bsc-testnet", output_token: "USDT", amount_out: "0.1"`
- *"Where is order 0xafa4…d55e? It was opened on bsc-testnet."* →
  `intent_track_order { order_id: "0xafa4…d55e", chain: "bsc-testnet" }`
- *"Refund my expired order 0xafa4…d55e (opened on bsc-testnet)."* →
  `intent_refund_order { order_id: "0xafa4…d55e", chain: "bsc-testnet" }`

Tips: tokens are symbols (`USDT`/`USDT0`→`USD`, `wCOEN`→`COEN`) or a raw `0x`
address;

Env: `OUTBE_INTENT_ROUTER` overrides the router address (same address on every
network; default the deployed testnet router `0x1619…a5b3`). Network RPCs and
token addresses live in `src/intent/`. Pass it like any other env var:

```bash
claude mcp add outbe --scope user \
  -e OUTBE_PRIVATE_KEY=0x... \
  -e OUTBE_INTENT_ROUTER=0x... \
  -- npx -y @outbe/mcp --rpc https://rpc.testnet.outbe.net
```

## Notes

- Contract registry, addresses and ABIs live in `src/registry.ts`. Source of truth:
  `contracts/precompiles/src/I*.sol` (+ `crates/blockchain/primitives/src/addresses.rs`).
  `ITeeRegistry` has no `.sol`; its ABI comes from `bin/outbe-cli/src/abi.rs`.
- ABIs are embedded as viem human-readable signatures (no Solidity compile step).
  If an interface changes, regenerate from `forge inspect <I>.sol abi` and update
  `registry.ts`. A method whose deployed shape differs from HEAD (node running an
  older/newer binary) will surface a viem decode error — align the ABI to the node.
- Crypto port mirrors `scripts/tribute_offer.py`, verified end-to-end against
  `rpc.testnet.outbe.net` (decryption reaches business logic; no AEAD failure).
- Intent domain logic lives in `src/intent/` (`registry`, `tokens`, `format`); the
  MCP tool registrations live in `src/tools/intent.ts`, alongside `view.ts` /
  `sign.ts`. Networks reuse the root `createCtx` (`src/chain.ts`) — a resolved
  network is just a thin view over a chain `Ctx`. Contract source of truth:
  `contracts/intent/` (router/order encoder) and `contracts/intent/examples/scripts/*`
  (reference user flow). The router/ERC20 ABIs are embedded as human-readable
  signatures, same as `registry.ts`.
