# Genesis protocol parameters

Outbe keeps production and testnet protocol parameters fixed at their canonical
Rust defaults. Test builds can read shorter timings from `genesis.json` so
LocalNet and E2E exercise the complete protocol in minutes without changing
runtime interfaces or state transitions.

## Resolution and startup ownership

1. A normal build reads the complete profile directly from a compile-time Rust
   constant. It has no protocol-parameter singleton or initialization-order
   requirement.
2. A normal build rejects a genesis containing `config.outbeProtocol`; the key
   is reserved for test binaries and cannot be silently ignored.
3. A build with `test-protocol-overrides` resolves optional
   `config.outbeProtocol` fields over those defaults and validates the complete
   value before execution startup.
4. Only that test build installs the resolved `GenesisProtocolParametersV1`
   once in a process-wide `OnceLock`.
5. Runtime modules read scalar values through explicit getters such
   as `get_metadosis_forming_period_seconds()`. They never read protocol
   parameters from EVM state.
6. OCOMP genesis tooling enforces the same build policy before it
   derives the capacity profile.

There is no runtime mutation API and no CLI or environment override. Malformed
test configuration is fatal. Production/testnet binaries contain no path that
uses JSON values and fail startup if the test-only key is present.

## Supported fields

| JSON path | Type and unit | Default | Validation | Consumer |
|---|---:|---:|---:|---|
| `metadosis.formingPeriodSeconds` | `u64`, seconds | 180000 | `1..=180000` | FORMING boundary |
| `metadosis.lookbackDelaySeconds` | `u64`, seconds | 1807200 | `0..=1807200` | LOOKBACK boundary |
| `metadosis.offeringPeriodSeconds` | `u64`, seconds | 180000 | `1..=180000` | Tribute offering window |
| `metadosis.waitingPeriodSeconds` | `u64`, seconds | 43200 | `1..=43200` | WAITING to READY |
| `metadosis.bootstrapDurationSeconds` | `u64`, seconds | 1814400 | `1..=1814400` | Metadosis bootstrap |
| `metadosis.advanceIntervalSeconds` | `u64`, seconds | 3600 | `1..=3600` | ProtocolCycle/WWD advancement cadence |
| `ocomp.computeVoteWindowBlocks` | `u64`, blocks | 1800 | `1..=1800` plus capacity gates | compute and vote deadline |
| `nodMaterialization.batchSubtreeHeight` | `u8`, tree levels | 3 | capacity `2^height` in `1..=256` | NOD batch capacity |
| `nodMaterialization.retryIntervalBlocks` | `u64`, blocks | 30 | nonzero | no-progress wake cadence |
| `nodMaterialization.maxAttemptsPerBlock` | `u16`, attempts | 1 | nonzero | execution attempt cap |

With `test-protocol-overrides`, unknown fields, unsupported schema versions,
overflow, and unsafe values fail genesis parsing. Missing fields use defaults
only in the central resolver; consumers cannot observe whether a value was
explicit or defaulted. In a feature build, runtime access before singleton
initialization fails immediately instead of silently returning defaults.
Normal builds have no singleton and therefore no such panic path.

## Production example

Production and testnet builds always select the complete default profile. Their
genesis must not contain the test-only `outbeProtocol` key.

```json
{
  "config": {
    "chainId": 54322346
  }
}
```

## LocalNet example

The LocalNet/E2E node binary must be built with
`--features test-protocol-overrides`.

```json
{
  "config": {
    "outbeProtocol": {
      "schemaVersion": 1,
      "metadosis": {
        "formingPeriodSeconds": 60,
        "lookbackDelaySeconds": 0,
        "offeringPeriodSeconds": 120,
        "waitingPeriodSeconds": 30,
        "bootstrapDurationSeconds": 300,
        "advanceIntervalSeconds": 10
      },
      "ocomp": {
        "computeVoteWindowBlocks": 120
      },
      "nodMaterialization": {
        "batchSubtreeHeight": 3,
        "retryIntervalBlocks": 12,
        "maxAttemptsPerBlock": 1
      }
    }
  }
}
```

`formingPeriodSeconds` is measured from the canonical UTC+14 start of the
WorldwideDay derived from block 1, not from process startup. The LocalNet harness
therefore resolves the absolute phase boundary relative to that canonical start.

The NOD materialization FIFO is initialized in genesis with
`head_sequence = tail_sequence = 1`, even when no legacy NOD is seeded.

## Network preparation order

```text
base and prefunds
  -> seed_genesis.py (ordinary state seed and optional overrides)
  -> OCOMP bindings, keys, and genesis install
  -> TEE genesis policy
  -> test node ChainSpec parse (resolve overrides and initialize the test singleton)
```

`outbeProtocol` is test input only. Production and testnet nodes reject it, so a
test genesis cannot accidentally boot with canonical production constants.
