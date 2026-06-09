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
quote = "0xUSD"
providers = ["mock"]

[[provider_endpoints]]
name = "mock_http"
rest = "https://prc.testnet.outbe.net"
websocket = "prc.testnet.outbe.net"

[[deviation_thresholds]]
base = "COEN"
threshold = 2.0
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
| `currency_pairs[].base` | yes | Base asset symbol |
| `currency_pairs[].quote` | yes | Quote asset symbol |
| `currency_pairs[].chain_denom` | no | Compatibility field for migrated Cosmos/test configs |
| `currency_pairs[].providers` | yes | Provider names listed below |
| `provider_endpoints[].name` | only endpoint-backed providers | Provider endpoint name |
| `provider_endpoints[].rest` | only endpoint-backed providers | Provider REST base URL |
| `provider_endpoints[].websocket` | no | Compatibility field for migrated Cosmos/test configs |
| `deviation_thresholds[].base` | no | Asset to apply threshold to |
| `deviation_thresholds[].threshold` | no | Max sigma deviation (default: 2.0) |

### Validation

At startup, the feeder validates:

- `vote_period > 0`
- `validator_address` is a valid 20-byte hex address
- Each pair has at least 1 provider
- All provider names are known: `mock`, `mock_http`, `pyth`, `chainlink`, `binance`, `kraken`, `okx`, `gate`, `huobi`, `mexc`, `coinbase`

## Providers

| Name | Status | Source |
|------|--------|--------|
| `mock` | Working | Hardcoded COEN=1.0, ETH=2500.0 |
| `mock_http` | Working | Configured REST endpoint compatible with the migrated Cosmos test price server |
| `pyth` | Working | Pyth Hermes REST API for supported BTC/ETH feeds |
| `chainlink` | Working | CryptoCompare REST API used as the Chainlink-compatible data source |
| `binance` | Working | Binance REST ticker/candle APIs |
| `kraken` | Working | Kraken REST ticker/candle APIs |
| `okx` | Working | OKX REST ticker/candle APIs |
| `gate` | Working | Gate.io REST ticker/candle APIs |
| `huobi` | Working | Huobi REST ticker/candle APIs |
| `mexc` | Working | MEXC REST ticker/candle APIs |
| `coinbase` | Working | Coinbase REST spot price API |

Provider errors, non-success responses, unsupported custom pairs, and timeouts are logged and skipped. The feeder does not fabricate fallback prices from failed providers.

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
