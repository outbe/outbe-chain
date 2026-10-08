# outbe-feeder

Outbe price oracle feeder daemon. Fetches prices from external providers, aggregates via VWAP, and submits oracle votes to the on-chain Oracle precompile.

## Quick Start

```bash
cargo build -p outbe-feeder
./target/debug/outbe-feeder --config feeder.toml
```

For production-like builds:

```bash
cargo build --release -p outbe-feeder
./target/release/outbe-feeder --config feeder.toml
```

## Configuration

TOML config file. Example:

```toml
[chain]
rpc_endpoint = "http://localhost:8545"
chain_id = 31337
gasless_oracle_votes = true

[account]
private_key = "0x..."
validator_address = "0x1111111111111111111111111111111111111111"

[oracle]
vote_period = 8
poll_interval_secs = 2

[health]
enabled = true
bind_address = "0.0.0.0:9002"

[[currency_pairs]]
base = "COEN"
quote = "840"

[[currency_pairs.sources]]
provider = "mock_http"
base = "COEN"
quote = "USDT"

[[currency_pairs.sources]]
provider = "mock_http"
base = "COEN"
quote = "USDC"

# A USD-quoted feed from the RedStone gateway; needs the [redstone] section.
[[currency_pairs.sources]]
provider = "redstone"
base = "USDC"
quote = "840"

# A market read from an on-chain feed contract; see [onchain_feeds] below.
[[currency_pairs]]
base = "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"   # WETH
quote = "840"

[[currency_pairs.sources]]
provider = "chainlink"
base = "ETH"
quote = "840"

[[provider_endpoints]]
name = "mock_http"
rest = "https://prc.testnet.outbe.net"

# RedStone gateway access, one key per validator.
[redstone]
api_key = "REPLACE_WITH_REDSTONE_API_KEY"

# Another oracle's on-chain feed contracts (Chainlink AggregatorV3 interface).
# The table name is the provider name for currency_pairs.sources; feeds are
# keyed by BASE/QUOTE.
[onchain_feeds.chainlink]
chain_id = 1
rpc_endpoint = "https://ethereum-rpc.example.invalid"

[onchain_feeds.chainlink.feeds."ETH/840"]
contract = "0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419"
description = "ETH / USD"

# Optional exchange WebSocket override. Omit it to use the exchange default.
[[provider_endpoints]]
name = "binance"
websocket = "wss://stream.binance.com:9443/ws"

[[deviation_thresholds]]
base = "COEN"
threshold = "2.0"
```

### Config Fields

| Field | Required | Description |
|-------|----------|-------------|
| `chain.rpc_endpoint` | yes | JSON-RPC HTTP endpoint |
| `chain.chain_id` | yes | Chain ID for transaction signing |
| `chain.gasless_oracle_votes` | no | Submit oracle votes through the system `zerofee` hook registry (default: false) |
| `account.private_key` | yes | Hex-encoded feeder private key used by alloy `PrivateKeySigner` |
| `account.validator_address` | yes | Validator this feeder acts for |
| `oracle.vote_period` | yes | Blocks per vote window (must match on-chain) |
| `oracle.poll_interval_secs` | no | Block polling interval (default: 2s) |
| `health.enabled` | no | Enables health/status HTTP server (default: true) |
| `health.bind_address` | no | Health server bind address (default: `0.0.0.0:9002`) |
| `currency_pairs[].base` | yes | On-chain base asset: `COEN`, an ISO 4217 numeric code, or a `0x` token address |
| `currency_pairs[].quote` | yes | On-chain quote asset in the same format |
| `currency_pairs[].sources` | yes | One or more external source markets aggregated into this on-chain pair |
| `currency_pairs[].sources[].provider` | yes | Provider name listed below |
| `currency_pairs[].sources[].base` | yes | Provider-market base symbol |
| `currency_pairs[].sources[].quote` | yes | Provider-market quote symbol |
| `provider_endpoints[].name` | only endpoint-backed providers | Provider endpoint name |
| `provider_endpoints[].rest` | only endpoint-backed providers | Provider REST base URL |
| `provider_endpoints[].websocket` | no | Exchange market-stream endpoint override (`ws://`, `wss://`, or a host); omitted uses the exchange default |
| `redstone.api_key` | only `redstone` sources | RedStone authenticated-gateway key, one per validator |
| `redstone.gateway` | no | RedStone gateway URL override |
| `dex_providers` | only DEX sources | Explicit RPC, network and pool configuration; see [DEX providers](#dex-providers) |
| `onchain_feeds.<name>` | only on-chain feed sources | One table per vendor and network; `<name>` is the provider name for `currency_pairs.sources`, see [On-chain feeds](#on-chain-feeds) |
| `onchain_feeds.<name>.feeds."BASE/QUOTE"` | with the table | `contract` and expected `description` of one feed |
| `deviation_thresholds[].base` | no | Asset to apply threshold to |
| `deviation_thresholds[].threshold` | no | Max sigma deviation as an exact decimal string (default: `"2.0"`) |

### Validation

At startup, the feeder validates:

- `vote_period > 0`
- `validator_address` is a valid 20-byte hex address
- Each on-chain pair has at least 1 external source market
- ISO markets use `COEN/ISO`; reverse `ISO/COEN` configuration is rejected
- All provider names are known: `mock`, `mock_http`, `pyth`, `redstone`, `binance`, `kraken`, `okx`, `gate`, `huobi`, `mexc`, `coinbase`, `uniswap`, `pancakeswap`, or a table name under `[onchain_feeds]`
- WebSocket endpoints are only accepted for streaming exchange providers
- Provider endpoint names are unique

Provider prices and volumes are parsed and aggregated as deterministic FP18
integers. Votes encode `COEN/ISO` price and volume at protocol scale `10^6`;
all other configured pairs retain scale `10^18` and their configured direction.
All source markets nested under one `currency_pairs` entry produce one vote
tuple: each source uses candle TVWAP when available and the ticker otherwise,
then the feeder deviation-filters the observations and combines them with a
volume-weighted mean rounded down.

## Providers

| Name | Status | Source |
|------|--------|--------|
| `mock` | Working | Hardcoded COEN=1.0, ETH=2500.0 |
| `mock_http` | Working | Configured REST endpoint compatible with the migrated Cosmos test price server |
| `pyth` | Working | Pyth Hermes REST API for supported BTC/ETH feeds |
| `<onchain_feeds.name>` | Implemented | Another oracle's on-chain feed contract (Chainlink, RedStone push; Chainlink `AggregatorV3Interface`) read over EVM JSON-RPC at `latest`; name chosen in config |
| `redstone` | Implemented | RedStone authenticated gateway: USD-quoted feeds, median of the 3 registered signers closest to the median |
| `binance` | Working | Binance WebSocket ticker/candle streams with REST bootstrap fallback |
| `kraken` | Working | Kraken WebSocket ticker/candle streams with REST bootstrap fallback |
| `okx` | Working | OKX WebSocket ticker/candle streams with REST bootstrap fallback |
| `gate` | Working | Gate.io WebSocket ticker/candle streams with REST bootstrap fallback |
| `huobi` | Working | Huobi WebSocket ticker/candle streams with REST bootstrap fallback |
| `mexc` | Working | MEXC protobuf WebSocket ticker/candle streams with REST bootstrap fallback |
| `coinbase` | Working | Coinbase WebSocket ticker stream with REST bootstrap fallback |
| `uniswap` | Implemented | Ethereum V2/V3/V4 pool spot rate and 1h COEN swap volume, read a few blocks behind the head |
| `pancakeswap` | Implemented | BNB Chain V2/V3/Infinity CL/Bin pool spot rate and 1h COEN swap volume, read a few blocks behind the head |

Provider errors, non-success responses, unsupported custom pairs, and timeouts are logged and skipped. The feeder does not fabricate fallback prices from failed providers.

Exchange providers connect to their market-data WebSocket, subscribe only to
their configured pairs, cache the latest ticker and recent candles, answer
protocol heartbeats, and reconnect with automatic resubscription. Until a
stream has produced data for a configured pair, its existing REST adapter is
used as bootstrap fallback.

### RedStone provider

`redstone` reads signed data packages from the RedStone authenticated gateway
(`/v2/data-packages/latest-by-data-feeds/redstone-primary-prod`). It needs a
`[redstone]` section with `api_key` (issued by RedStone, one per validator)
and optionally `gateway` to override the gateway URL. Feeds are USD quoted,
so a source must use quote `840` or `USD`; the base symbol is the RedStone
feed id (`USDC`, `USDT`, `ETH`).

Selection follows the RedStone SDK defaults. For each feed the provider keeps
the packages that share the newest timestamp and come from a signer registered
for `redstone-primary-prod` (the registry list is compiled in and dated in
`provider/redstone.rs`), requires three distinct signers, takes the three
values closest to the median, and publishes their median. Packages older than
60 seconds or more than 30 seconds in the future reject the feed. Package
signatures are not verified; the gateway is trusted like Pyth Hermes.
RedStone lists no COEN feed. See the configuration example above for the
`[redstone]` section and a `redstone` source.
### On-chain feeds

`[onchain_feeds.<name>]` tables read price feeds from another oracle's
on-chain contract: Chainlink Data Feeds, RedStone push feeds, and any vendor
whose feed contract exposes Chainlink's `AggregatorV3Interface`
(`latestRoundData()`, `decimals()`, `description()`). Each table is one
provider instance and `<name>` is the provider name used in
`currency_pairs.sources`, so a feeder can hold the same market from several
vendors as separate sources and let the deviation filter and source mean work
across them. Feeds are keyed by `BASE/QUOTE`; a source whose provider is a
table name must match a feed key. See the configuration example above.

Table names may not reuse a built-in provider name; contract addresses are
unique within a table. Take `contract` (the proxy address) and `description`
from the vendor registry:
[docs.chain.link](https://docs.chain.link/data-feeds/price-feeds/addresses) or
[app.redstone.finance](https://app.redstone.finance/). Reads are `eth_call`
against the `latest` block: rounds are signed by the vendor network, so no
finality wait applies; freshness comes from the round's `updatedAt`. On first
use the feeder verifies `eth_chainId`, reads `description()` and rejects a
feed whose text differs from the configured one, then caches `decimals()`.
Each vote reads `latestRoundData()` per feed. A round is skipped when `answer`
is not positive, `updatedAt` is in the future, or it is older than 25 hours:
the longest documented heartbeat at either vendor is 24 hours, so a round
older than that means the relayer stopped. Feeds report no volume, so the
observation weighs one unit in the source mean. Neither vendor lists a COEN
feed today; a COEN push feed, once deployed, is configured here like any other.

### DEX providers

DEX v1 reads the current pool spot rate and independently collects one-hour
COEN volume, matching the 60 one-minute candles exchange sources aggregate. It returns tickers, with no candles or time averaging of the rate.
All configured `COEN/USDC` and `COEN/USDT` source markets feed the same
`COEN/840` Oracle pair through the existing deviation filter and volume-weighted
source mean. **V1 assumes USDC = USDT = 1 USD.** There is no stablecoin/USD feed
or depeg correction. The rate is the core AMM price before fees, slippage or
hook-specific trade adjustments. Nonzero hooks are supported; volume describes
core `Swap` balance deltas, not additional hook transfers or router turnover.

Replace the illustrative token and pool addresses, RPC URLs, destination chain
settings and signing account before running it. COEN pool deployment and
live-chain validation are separate from this example. Existing configurations
continue to work without a `dex_providers` section.

Each `[[dex_providers]]` contains:

| Field | Meaning |
|---|---|
| `name`, `chain_id` | `uniswap`, `1` (Ethereum), or `pancakeswap`, `56` (BNB Chain) |
| `rpc_endpoint` | HTTP(S) JSON-RPC; separate from the destination `[chain]` RPC |
| `poll_interval_secs` | Poll/retry delay, default 2 seconds, allowed 1–10 |
| `log_chunk_blocks` | Maximum blocks per log request, default 2000, allowed 1–10000; errors reduce the range down to one block |
| `confirmations` | Blocks behind `latest` to read from, default 3, allowed 0–1000; zero reads the head |
| `max_block_age_secs` | Maximum wall-clock age of the block read, default 1800 seconds (`max_finalized_age_secs` is accepted as an alias) |
| `markets` | Explicit `base`, `quote`, `base_token`, `quote_token`, and nested `pool` configuration |

One pool is selected explicitly per `(provider, base, quote)`; duplicate markets
and duplicate pool identities are rejected. There is no automatic pool discovery
or liquidity-based switching. Both tokens must be ERC20 contracts. Their
`decimals()` are read on-chain (0–77 supported); V2/V3 token addresses are checked
against `token0()` and `token1()`. Symbols never select a token contract.

The nested `[dex_providers.markets.pool]` accepts these variants:

| `protocol` | Required pool fields | Rate read |
|---|---|---|
| `uniswap_v2`, `pancakeswap_v2` | `address` | `getReserves()`, quote/base reserve ratio |
| `uniswap_v3`, `pancakeswap_v3` | `address` | `slot0().sqrtPriceX96`, squared / 2^192 |
| `uniswap_v4` | `manager`, `state_view`, `fee`, `tick_spacing`, `hooks` | `StateView.getSlot0(poolId)` |
| `infinity_cl` | `manager`, `fee`, `hooks`, `parameters` | `CLPoolManager.getSlot0(poolId)` |
| `infinity_bin` | `manager`, `fee`, `hooks`, `parameters` | Active bin price from `activeId` and `binStep` |

V4/Infinity pool IDs are derived from the full key: sorted token addresses and
the configured fields above. V4 verifies that `StateView.poolManager()` matches
`manager`. Infinity `parameters` is a 32-byte hex value from the actual pool key:
the lower 16 bits contain the hook bitmap; the next 24 bits contain CL tick
spacing, or the next 16 bits contain Bin step. Copy the actual creation key,
including its hook settings and fee. A dynamic fee is encoded as `8388608`.

RPC must support `eth_chainId`, `eth_getBlockByNumber("latest")`, historical
block headers, `eth_getLogs`, and EIP-1898 `eth_call` with
`{blockHash, requireCanonical: true}`. Each cycle reads `latest`, steps back
`confirmations` blocks by number, and pins every price, decimals and log read to
that block's hash; volume covers `(block timestamp - 1h, block timestamp]`.
The feeder does not wait for the `finalized` tag: on Ethereum that lags about
13 minutes and moves once per epoch, while three confirmations already rule
out ordinary one-block reorgs. A deeper reorg still fails the hash checks and
rebuilds the volume window. Some [public BNB RPCs disable eth_getLogs](https://docs.bnbchain.org/bnb-smart-chain/developers/json_rpc/json-rpc-endpoint/);
use an endpoint that exposes it and returns complete results or a range-limit
error. Oversized responses above 16 MiB are rejected and log ranges reduced.

Each market has an independent background worker. It backfills the volume
window at startup, then scans only new confirmed blocks. It deduplicates log
rows, checks block hashes and commits a range only after validating every event.
V2 volume uses the absolute net base-token input/output; V3/V4/Infinity use the
absolute signed base-token delta. Volumes remain in COEN units for comparable
weights across USDC/USDT markets. Only per-block sums are retained in memory;
restart rebuilds the window from RPC, and detected chain-history changes
clear it for rebuilding.

Uninitialized pools, RPC failures and incomplete backfills publish no ticker.
A complete window with no swaps publishes real zero volume. Cached observations
expire 30 seconds after their state acquisition began; a long backfill cannot
make an old spot price look freshly acquired. A failed market does not block
other markets. Dropping the provider cancels its workers. Logs distinguish
warmup, a ready block/hash and retryable unavailability.

ABI and price formula references:
[Uniswap V2](https://github.com/Uniswap/v2-core/blob/master/contracts/interfaces/IUniswapV2Pair.sol),
[Uniswap V3](https://github.com/Uniswap/v3-core/blob/main/contracts/interfaces/pool/IUniswapV3PoolState.sol),
[PancakeSwap V3 events](https://github.com/pancakeswap/pancake-v3-contracts/blob/main/projects/v3-core/contracts/interfaces/pool/IPancakeV3PoolEvents.sol),
[Uniswap V4 StateView](https://github.com/Uniswap/v4-periphery/blob/main/src/lens/StateView.sol),
[Infinity CL](https://github.com/pancakeswap/infinity-core/blob/main/src/pool-cl/interfaces/ICLPoolManager.sol),
[Infinity Bin PriceHelper](https://github.com/pancakeswap/infinity-core/blob/main/src/pool-bin/libraries/PriceHelper.sol).

For the migrated price-oracle testnet config and launcher, bootstrap a local
testnet with oracle genesis params, start the node, then run one feeder. Do not
set `PRICE_REST_URL` for the normal remote price endpoint; `run.sh` uses
`https://prc.testnet.outbe.net` from `scripts/price-oracle/config.toml` by
default:

```bash
./scripts/bootstrap-testnet.sh 4 /tmp/outbe-testnet
./scripts/run-testnet.sh start /tmp/outbe-testnet
./scripts/price-oracle/run.sh /tmp/outbe-testnet 0
```

`PRICE_REST_URL` is only an override for replacing the configured REST endpoint.
Use it only when a local mock price server is already running:

```bash
PRICE_REST_URL=http://localhost:8000 ./scripts/price-oracle/run.sh /tmp/outbe-testnet 0
```

## Architecture

1. Reads the latest canonical block and derives its vote period as `height / vote_period`.
2. Pins oracle and validator preflight reads to that block hash. A failed read is retried on the next poll, without consuming the period.
3. Checks the signer, active validator, oracle settings, and whether the validator has already voted.
4. Fetches provider prices, aggregates them, and repeats preflight if the period changed during aggregation.
5. Signs a transaction with an explicit nonce and writes its raw bytes to a durable journal before broadcasting.
6. Rechecks the canonical receipt and nonce on each poll. A lost RPC response is retried with the same bytes; a stalled transaction can be replaced at the same nonce with a higher fee.
7. Reports canonical head progress, oracle price freshness, pending transaction age, and vote results through `/health` and `/status`.

## Oracle Precompile

Address: `0x000000000000000000000000000000000000EE05`

The feeder submits votes via standard EVM transactions to this address. See `interfaces/IOracle.sol` for the full ABI.

## Signing Path

`account.private_key` is parsed once at startup:

```text
PrivateKeySigner::parse()
  -> EthereumWallet::from()
  -> sign EIP-1559 transaction with explicit nonce
  -> persist raw transaction and hash
  -> eth_sendRawTransaction
```

`chain.chain_id` is set explicitly on each vote transaction. The journal is keyed by chain genesis, signer, and validator, and is stored in `STATE_DIRECTORY` when systemd supplies it. The validator service provisions `/var/lib/outbe-feeder` for this purpose. Without systemd, the journal is stored beside the canonical config path.

When `chain.gasless_oracle_votes = true`, the feeder still sends a normal signed EVM transaction to `Oracle.submitVote(...)`, but marks it with zero priority fee and a max fee cap high enough for Reth's public txpool protocol checks. The `outbe-txpool` crate and executor both call the system `zerofee` hook registry. The registered `OracleSubmitVoteHook` revalidates the signer, delegated feeder status, one-vote-per-period rule, zero native value, and policy size limits before waiving native fee debit. Authorized gasless `submitVote` transactions are ordered ahead of fee-paying transactions inside the Outbe txpool, so payload building considers validator votes before the normal tip market while still enforcing nonce, validity, and block gas limits. Paid `submitVote` transactions keep the normal EVM path.

## Feeder Delegation

A validator can delegate vote submission to a separate feeder account:

```
IOracle.delegateFeederConsent(feederAddress)
```

The feeder then signs transactions with its own key but votes count for the delegating validator.

## Health Checks

Default bind address: `0.0.0.0:9002`.

```bash
curl -s http://127.0.0.1:9002/health
curl -s http://127.0.0.1:9002/status
```

`/health` returns HTTP 200 when the feeder is healthy and HTTP 503 when unhealthy. `/status` includes the latest observed period, vote counters, pending transaction, head progress, and per-pair oracle freshness.
