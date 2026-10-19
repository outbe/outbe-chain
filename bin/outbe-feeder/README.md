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

### Sources mode

`[oracle] sources` in `feeder.toml` selects where the pairs and their markets
come from:

| Value | Where | Use for |
|---|---|---|
| `chain` (default) | The node: `IOracle.getSources()` | mainnet |
| `local` | `feeder.toml`, or the file that `pairs` names | localnet, testnet, e2e |

In `chain` mode `feeder.toml` must not set `currency_pairs`,
`deviation_thresholds`, `dex_providers` or `onchain_providers`.

The node files are in `crates/system/oracle/sources/<major>.<minor>.toml`.
They are for mainnet only. Other networks register pairs in genesis. The node
serves the file with the highest version that is not above the active protocol
version. The node also registers the pairs of that file and stops voting on
pairs that a newer file drops. To change the sources, add a new file named by
the protocol version of the release. Do not edit a released file.

The feeder votes only pairs that are active vote targets on chain
(`IOracle.getVoteTargets()`).

The node file has no RPC URLs and no API keys. Set them in `feeder.toml`:
`[rpc_endpoints]` by `chain_id`, and `[[api_providers]]`.

### `feeder.toml`, `chain` mode

```toml
[chain]
rpc_endpoint = "http://localhost:8545"
chain_id = 31337
gasless_oracle_votes = true

[account]
private_key = "0x..."
validator_address = "0x1111111111111111111111111111111111111111"

[oracle]
vote_period = 60
poll_interval_secs = 2

[health]
enabled = true
bind_address = "0.0.0.0:9002"

# JSON-RPC of each external EVM network, by chain id.
[rpc_endpoints]
1 = "https://ethereum-rpc.example.invalid"

# Connection settings only. Omit `rest` or `websocket` for the default.
[[api_providers]]
name = "pyth"
api_key = "REPLACE_WITH_PYTH_API_KEY"

# Optional: without a key, redstone reads the public gateway.
[[api_providers]]
name = "redstone"
api_key = "REPLACE_WITH_REDSTONE_API_KEY"   # one per validator
```

### `local` mode

Add to the `[oracle]` section above:

```toml
sources = "local"
```

Put the sources in `feeder.toml`, or in a second file with a top-level
`pairs = "pairs.toml"`.

`pairs.toml`:

```toml
# The Oracle pairs to vote on. Each pair lists its own markets. A market is
# written in the provider's own symbols. The feeder substitutes nothing.
# Every market price joins one volume-weighted mean per pair.
[[currency_pairs]]
base = "COEN"
quote = "840"
markets = [
  { provider = "binance", quote = "USDT" },
  { provider = "uniswap" },
]

# COEN in euros, a cross rate: COEN/978 = COEN/840 ÷ 978/840. The markets
# give dollars per euro. The feeder votes only COEN/978, with zero volume.
[[currency_pairs]]
base = "COEN"
quote = "978"
cross = "840"
markets = [
  { provider = "chainlink_eur_usd" },
  { provider = "pyth" },
  { provider = "redstone" },
]

# COEN in yen, the same way: COEN/392 = COEN/840 ÷ 392/840.
[[currency_pairs]]
base = "COEN"
quote = "392"
cross = "840"
markets = [
  { provider = "chainlink_jpy_usd" },
  { provider = "pyth" },
]

[[deviation_thresholds]]
base = "COEN"
threshold = "2.0"

[[dex_providers]]
name = "uniswap"
chain_id = 1
base_token = "0x1111111111111111111111111111111111111111"    # COEN token, replace
quote_token = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"   # USDC
pool = { protocol = "uniswap_v3", address = "0x2222222222222222222222222222222222222222" }   # replace

[[onchain_providers]]
name = "chainlink_eur_usd"
chain_id = 1
contract = "0x4444444444444444444444444444444444444444"   # Chainlink EUR / USD, replace

[[onchain_providers]]
name = "chainlink_jpy_usd"
chain_id = 1
contract = "0x5555555555555555555555555555555555555555"   # Chainlink JPY / USD, replace
```

### Config Fields

| Field | Required | Description |
|-------|----------|-------------|
| `pairs` | no | Local sources only. Path of the pairs file, relative to this file. Its sections join the config |
| `chain.rpc_endpoint` | yes | JSON-RPC HTTP endpoint |
| `chain.chain_id` | yes | Chain ID for transaction signing |
| `chain.gasless_oracle_votes` | no | Submit oracle votes through the system `zerofee` hook registry (default: false) |
| `account.private_key` | yes | Hex-encoded feeder private key used by alloy `PrivateKeySigner` |
| `account.validator_address` | yes | Validator this feeder acts for |
| `oracle.vote_period` | yes | Blocks per vote window (must match on-chain) |
| `oracle.poll_interval_secs` | no | Block polling interval (default: 2s) |
| `oracle.sources` | no | `chain` (default): sources from the node. `local`: sources from the local config |
| `rpc_endpoints.<chain_id>` | for each external network in the sources | JSON-RPC URL of that EVM network |
| `health.enabled` | no | Enables health/status HTTP server (default: true) |
| `health.bind_address` | no | Health server bind address (default: `0.0.0.0:9002`) |
| `currency_pairs[].base` | yes | On-chain base asset: `COEN`, an ISO 4217 numeric code, or a `0x` token address |
| `currency_pairs[].quote` | yes | On-chain quote asset in the same format |
| `currency_pairs[].cross` | no | Cross currency (ISO code) for a pair with an ISO quote: the price is `<base>/<cross>` ÷ `<quote>/<cross>`. The markets then price `quote` in `cross`, and a direct `<base>/<cross>` pair must exist |
| `currency_pairs[].markets[].provider` | yes | Built-in provider name or an `onchain_providers` entry name |
| `currency_pairs[].markets[].quote` | exchanges only | Quote symbol the provider prices the pair base in. Omitted takes `cross` if set, else the pair's quote. An exchange needs its own symbol (`USDT`, `USDC`, `USD`). For an ISO pair quote it must name that currency: `USDT` and `USDC` are `840`, and an ISO code (`USD`, `840`, `EUR`, `978`) is that currency |
| `currency_pairs[].markets[].base` | no | The provider's name for the pair base, when it differs (default: the pair's `base`) |
| `api_providers[].name` | only to change a default or set a key | Built-in provider name |
| `api_providers[].rest` | no | Provider REST base URL; for `redstone` the gateway URL. Omitted uses the provider default |
| `api_providers[].websocket` | no | Exchange market-stream endpoint override (`ws://`, `wss://`, or a host); omitted uses the exchange default |
| `api_providers[].api_key` | `pyth`: yes, `redstone`: no | Gateway key, one per validator. `pyth` needs one. Without one `redstone` reads its public gateway |
| `dex_providers[]` | only DEX markets | One entry is one pool; see [DEX providers](#dex-providers) |
| `onchain_providers[].name` | only on-chain feeds | Provider name the operator chooses; one entry is one feed, see [On-chain feeds](#on-chain-feeds) |
| `onchain_providers[].chain_id` | with the entry | EVM network of the feed contract |
| `onchain_providers[].rpc_endpoint` | no | JSON-RPC of the feed contract. Omitted takes `rpc_endpoints.<chain_id>` |
| `onchain_providers[].contract` | with the entry | Feed contract, the vendor's proxy address |
| `deviation_thresholds[].base` | no | Asset to apply threshold to |
| `deviation_thresholds[].threshold` | no | Max sigma deviation as an exact decimal string (default: `"2.0"`) |

### Validation

At startup, the feeder validates:

- `vote_period > 0`
- `validator_address` is a valid 20-byte hex address
- Each on-chain pair lists at least one market, and a market quote names the
  pair's ISO quote currency
- Each `dex_providers` and `onchain_providers` entry is named by exactly one
  pair market
- ISO markets use `COEN/ISO`; reverse `ISO/COEN` configuration is rejected
- All provider names are known: `mock`, `mock_http`, `pyth`, `redstone`, `binance`, `kraken`, `okx`, `gate`, `huobi`, `mexc`, `coinbase`, `uniswap`, `pancakeswap`, or an `onchain_providers` entry name
- WebSocket endpoints are only accepted for streaming exchange providers
- An exchange market is quoted in the exchange's symbol (`USDT`, `USDC`, `USD`),
  never an ISO code: the feeder substitutes no market for a currency
- Currency rates (`978/840`, `cross` markets) come only from `pyth`,
  `redstone` and `onchain_providers`, not from exchanges or DEX pools
- A `cross` pair needs the direct `<base>/<cross>` pair
- API provider names are unique

Provider prices and volumes are parsed and aggregated as deterministic FP18
integers. Votes encode `COEN/ISO` price and volume at protocol scale `10^6`;
all other configured pairs retain scale `10^18` and their configured direction.
All markets attached to one `currency_pairs` entry produce one vote tuple:
each market uses candle TVWAP when available and the ticker otherwise,
then the feeder deviation-filters the observations and combines them with a
volume-weighted mean rounded down.

## Providers

| Name | Status | Source |
|------|--------|--------|
| `mock` | Working | Hardcoded COEN=1.0, ETH=2500.0 |
| `mock_http` | Working | Configured REST endpoint compatible with the migrated Cosmos test price server |
| `pyth` | Implemented | Pyth Hermes REST API with a key: FX and crypto feeds in dollars |
| `<onchain_providers.name>` | Implemented | Another oracle's on-chain feed contract (Chainlink, RedStone push; Chainlink `AggregatorV3Interface`) read over EVM JSON-RPC at `latest`; name chosen in config |
| `redstone` | Implemented | RedStone gateway, authenticated or public: USD-quoted feeds, median of the 3 registered signers closest to the median |
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

`redstone` reads signed data packages from a RedStone gateway. Two modes:

- With `api_key` (issued by RedStone, one per validator) in its
  `api_providers` entry: the authenticated gateway
  (`/v2/data-packages/latest-by-data-feeds/redstone-primary-prod`) returns
  only the requested feeds.
- Without a key: the public gateway `oracle-gateway-1.a.redstone.finance`
  (`/data-packages/latest/redstone-primary-prod`) returns every feed, about
  2 MB per vote. RedStone may retire it.

`rest` overrides the gateway URL in both modes. Feeds
are USD quoted, so a market must use quote `840` or `USD`; the base symbol is
the RedStone feed id (`USDC`, `USDT`, `ETH`). For an ISO numeric base the
feed id is its alpha code: pair `978/840` reads feed `EUR`.

Selection follows the RedStone SDK defaults. For each feed the provider keeps
the packages that share the newest timestamp and come from a signer registered
for `redstone-primary-prod` (the registry list is compiled in and dated in
`provider/redstone.rs`), requires three distinct signers, takes the three
values closest to the median, and publishes their median. Packages older than
60 seconds or more than 30 seconds in the future reject the feed. Package
signatures are not verified; the gateway is trusted like Pyth Hermes.
RedStone lists no COEN feed. See the configuration example above for the
`redstone` entry.
### Pyth provider

`pyth` reads Pyth Hermes (`https://pyth.dourolabs.app/hermes`). It needs
`api_key` in its `api_providers` entry
([pythdata.app](https://pythdata.app/signup)).

Pair `978/840` reads feed `FX.EUR/USD`. A feed Pyth quotes the other way
(`FX.USD/JPY`) is inverted to dollars per unit. `base` names another feed:
`base = "FX.USD/CNH"`.

### On-chain feeds

`[[onchain_providers]]` entries read price feeds from another oracle's
on-chain contract: Chainlink Data Feeds, RedStone push feeds, and any vendor
whose feed contract exposes Chainlink's `AggregatorV3Interface`
(`latestRoundData()`, `decimals()`). One entry is one feed
contract on one network; `name` is a provider name the operator chooses, and
one pair market names it. The market's `quote` is the currency the feed prices
the pair base in. Its `base` names the pair base the vendor's way when it
differs, for example `ETH` for a WETH pair. The same market from a second
vendor is a second entry under another name, so the deviation filter and
source mean work across vendors. See the configuration
example above.

Entry names may not reuse a built-in provider name. Take `contract` (the
proxy address) from the vendor registry:
[docs.chain.link](https://docs.chain.link/data-feeds/price-feeds/addresses) or
[app.redstone.finance](https://app.redstone.finance/). The feeder does not
check the contract against the market it is listed under; the operator is
responsible for the address. Reads are `eth_call` against the `latest` block:
rounds are signed by the vendor network, so no finality wait applies;
freshness comes from the round's `updatedAt`. On first use the feeder
verifies `eth_chainId` and caches `decimals()`.
Each vote reads `latestRoundData()` per feed. A round is skipped when `answer`
is not positive, `updatedAt` is in the future, or it is older than 25 hours:
the longest documented heartbeat at either vendor is 24 hours, so a round
older than that means the relayer stopped. Feeds report no volume, so the
observation weighs one unit in the source mean. Neither vendor lists a COEN
feed today; a COEN push feed, once deployed, is configured here like any other.

### DEX providers

DEX v1 reads the current pool spot rate and independently collects one-hour
base volume, matching the 60 one-minute candles exchange sources aggregate. It returns tickers, with no candles or time averaging of the rate.
A pool feeds the Oracle pair whose market names it, like any other market, through
the existing deviation filter and volume-weighted source mean. There is no
stablecoin/USD feed or depeg correction. The rate is the core AMM price before fees, slippage or
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
| `rpc_endpoint` | HTTP(S) JSON-RPC, separate from the destination `[chain]` RPC. Omitted takes `rpc_endpoints.<chain_id>` |
| `poll_interval_secs` | Poll/retry delay, default 2 seconds, allowed 1–10 |
| `log_chunk_blocks` | Maximum blocks per log request, default 2000, allowed 1–10000; errors reduce the range down to one block |
| `confirmations` | Blocks behind `latest` to read from, default 3, allowed 0–1000; zero reads the head |
| `max_block_age_secs` | Maximum wall-clock age of the block read, default 1800 seconds (`max_finalized_age_secs` is accepted as an alias) |
| `base_token`, `quote_token`, `pool` | The two ERC20 contracts and the pool this entry reads |

One entry is one pool. V1 reads one pool per DEX. There is no automatic pool
discovery or liquidity-based switching. Both tokens must be ERC20 contracts.
Their `decimals()` are read on-chain (0–77 supported); V2/V3 token addresses
are checked against `token0()` and `token1()`. Symbols never select a token
contract.

The entry's `pool` accepts these variants:

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
