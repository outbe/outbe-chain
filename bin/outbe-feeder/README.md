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

[[provider_endpoints]]
name = "mock_http"
rest = "https://prc.testnet.outbe.net"

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
| `dex_providers` | only DEX sources | Explicit RPC, network and pool configuration; see [DEX providers](#dex-providers) |
| `deviation_thresholds[].base` | no | Asset to apply threshold to |
| `deviation_thresholds[].threshold` | no | Max sigma deviation as an exact decimal string (default: `"2.0"`) |

### Validation

At startup, the feeder validates:

- `vote_period > 0`
- `validator_address` is a valid 20-byte hex address
- Each on-chain pair has at least 1 external source market
- ISO markets use `COEN/ISO`; reverse `ISO/COEN` configuration is rejected
- All provider names are known: `mock`, `mock_http`, `pyth`, `chainlink`, `binance`, `kraken`, `okx`, `gate`, `huobi`, `mexc`, `coinbase`, `uniswap`, `pancakeswap`
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
| `chainlink` | Working | CryptoCompare REST API used as the Chainlink-compatible data source |
| `binance` | Working | Binance WebSocket ticker/candle streams with REST bootstrap fallback |
| `kraken` | Working | Kraken WebSocket ticker/candle streams with REST bootstrap fallback |
| `okx` | Working | OKX WebSocket ticker/candle streams with REST bootstrap fallback |
| `gate` | Working | Gate.io WebSocket ticker/candle streams with REST bootstrap fallback |
| `huobi` | Working | Huobi WebSocket ticker/candle streams with REST bootstrap fallback |
| `mexc` | Working | MEXC protobuf WebSocket ticker/candle streams with REST bootstrap fallback |
| `coinbase` | Working | Coinbase WebSocket ticker stream with REST bootstrap fallback |
| `uniswap` | Implemented | Ethereum finalized V2/V3/V4 pool spot rate and 24h COEN swap volume |
| `pancakeswap` | Implemented | BNB Chain finalized V2/V3/Infinity CL/Bin pool spot rate and 24h COEN swap volume |

Provider errors, non-success responses, unsupported custom pairs, and timeouts are logged and skipped. The feeder does not fabricate fallback prices from failed providers.

Exchange providers connect to their market-data WebSocket, subscribe only to
their configured pairs, cache the latest ticker and recent candles, answer
protocol heartbeats, and reconnect with automatic resubscription. Until a
stream has produced data for a configured pair, its existing REST adapter is
used as bootstrap fallback.

### DEX providers

DEX v1 reads the current pool spot rate and independently collects 24-hour
COEN volume. It returns tickers, with no candles or time averaging of the rate.
All configured `COEN/USDC` and `COEN/USDT` source markets feed the same
`COEN/840` Oracle pair through the existing deviation filter and volume-weighted
source mean. **V1 assumes USDC = USDT = 1 USD.** There is no stablecoin/USD feed
or depeg correction. The rate is the core AMM price before fees, slippage or
hook-specific trade adjustments. Nonzero hooks are supported; volume describes
core `Swap` balance deltas, not additional hook transfers or router turnover.

Start with [dex.example.toml](dex.example.toml). Replace the illustrative token
and pool addresses, RPC URLs, destination chain settings and signing account
before running it. COEN pool deployment and live-chain validation are separate
from this example. Existing configurations continue to work without a
`dex_providers` section.

Each `[[dex_providers]]` contains:

| Field | Meaning |
|---|---|
| `name`, `chain_id` | `uniswap`, `1` (Ethereum), or `pancakeswap`, `56` (BNB Chain) |
| `rpc_endpoint` | HTTP(S) JSON-RPC; separate from the destination `[chain]` RPC |
| `poll_interval_secs` | Poll/retry delay, default 2 seconds, allowed 1–10 |
| `log_chunk_blocks` | Maximum blocks per log request, default 2000, allowed 1–10000; errors reduce the range down to one block |
| `max_finalized_age_secs` | Maximum wall-clock age of the finalized block, default 1800 seconds, including the chain's finality delay |
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

RPC must support `eth_chainId`, `eth_getBlockByNumber("finalized")`, historical
block headers, `eth_getLogs`, and EIP-1898 `eth_call` with
`{blockHash, requireCanonical: true}`. Price and decimals reads use one finalized
block hash; volume covers `(block timestamp - 24h, block timestamp]`. There is no
fallback to `latest`. Some [public BNB RPCs disable eth_getLogs](https://docs.bnbchain.org/bnb-smart-chain/developers/json_rpc/json-rpc-endpoint/);
use an endpoint that exposes it and returns complete results or a range-limit
error. Oversized responses above 16 MiB are rejected and log ranges reduced.

Each market has an independent background worker. It backfills the volume
window at startup, then scans only new finalized blocks. It deduplicates log
rows, checks block hashes and commits a range only after validating every event.
V2 volume uses the absolute net base-token input/output; V3/V4/Infinity use the
absolute signed base-token delta. Volumes remain in COEN units for comparable
weights across USDC/USDT markets. Only per-block sums are retained in memory;
restart rebuilds the window from RPC, and detected finalized-history changes
clear it for rebuilding.

Uninitialized pools, RPC failures and incomplete backfills publish no ticker.
A complete window with no swaps publishes real zero volume. Cached observations
expire 30 seconds after their state acquisition began; a long backfill cannot
make an old spot price look freshly acquired. A failed market does not block
other markets. Dropping the provider cancels its workers. Logs distinguish
warmup, a ready finalized block/hash and retryable unavailability.

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

1. Polls `eth_blockNumber` at configured interval
2. Detects a vote period boundary with `(height + 1) / vote_period > last_voted_period`
3. Runs read-only preflight before fetching prices:
   - `IOracle.getParams()` verifies the oracle is enabled and on-chain `votePeriod` matches local config
   - `IOracle.getVotePenaltyCounter(validator)` reads current oracle counters for logging/context
   - `IOracle.getAggregateVote(validator)` skips if a vote already exists for the current period
   - `IValidatorSet.validatorByAddress(validator)` reads lifecycle status for observability
4. If preflight fails, logs the reason and skips the period without building or sending a transaction
5. Fetches prices from configured providers
6. Filters outlier prices (sigma-based deviation filtering)
7. Computes VWAP (ticker) or TVWAP (candle, preferred)
8. Builds ABI-encoded `submitVote(ExchangeRateTuple[])` calldata
9. Signs with feeder private key via alloy and submits to Oracle precompile (`0xEE05`)
10. Records health success/failure state

## Oracle Precompile

Address: `0x000000000000000000000000000000000000EE05`

The feeder submits votes via standard EVM transactions to this address. See `interfaces/IOracle.sol` for the full ABI.

## Signing Path

`account.private_key` is parsed once at startup:

```text
PrivateKeySigner::parse()
  -> EthereumWallet::from()
  -> ProviderBuilder::new().wallet(wallet).connect_http(...)
  -> provider.send_transaction(tx)
```

`chain.chain_id` is set explicitly on each vote transaction. Alloy handles nonce lookup, gas estimation, signing, and broadcasting.

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

`/health` returns HTTP 200 when the feeder is healthy and HTTP 503 when unhealthy. `/status` returns JSON with the latest period, vote timestamp, success/failure counters, and configured vote period.
