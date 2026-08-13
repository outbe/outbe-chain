# Genesis protocol parameters

Outbe stores network-specific but immutable protocol parameters in
`genesis.json`. LocalNet can therefore exercise the complete protocol in minutes
without changing production code or state transitions.

## Resolution and storage

1. `config.outbeProtocol` contains optional overrides.
2. `outbe-chain constants genesis` resolves every missing field to its canonical
   default, validates the complete value, and writes it to immutable account
   `0x000000000000000000000000000000000000ee11`.
3. The account is covered by the genesis state root and genesis hash.
4. OCOMP validator registrations and bindings are created only after this step.
5. Runtime loads and validates the complete record once, then caches the
   immutable `GenesisProtocolParametersV1` by genesis hash.

There is no runtime mutation API and no CLI or environment override. Missing,
malformed, hash-mismatched, or incompatible materialized storage is fatal.

## Supported fields

| JSON path | Type and unit | Default | Validation | Consumer |
|---|---:|---:|---:|---|
| `metadosis.formingPeriodSeconds` | `u64`, seconds | 180000 | `1..=180000` | FORMING boundary |
| `metadosis.lookbackDelaySeconds` | `u64`, seconds | 1807200 | `0..=1807200` | LOOKBACK boundary |
| `metadosis.offeringPeriodSeconds` | `u64`, seconds | 180000 | `1..=180000` | Tribute offering window |
| `metadosis.waitingPeriodSeconds` | `u64`, seconds | 43200 | `1..=43200` | WAITING to READY |
| `metadosis.bootstrapDurationSeconds` | `u64`, seconds | 1814400 | `1..=1814400` | Metadosis bootstrap |
| `metadosis.advanceIntervalSeconds` | `u64`, seconds | 43200 | `1..=43200` | WWD advancement cadence |
| `ocomp.computeVoteWindowBlocks` | `u64`, blocks | 1800 | `1..=1800` plus capacity gates | compute and vote deadline |
| `nodMaterialization.batchSubtreeHeight` | `u8`, tree levels | 3 | capacity `2^height` in `1..=256` | NOD batch capacity |
| `nodMaterialization.retryIntervalBlocks` | `u64`, blocks | 30 | nonzero | no-progress wake cadence |
| `nodMaterialization.maxAttemptsPerBlock` | `u16`, attempts | 1 | nonzero | execution attempt cap |

Unknown fields, unsupported schema versions, overflow, and unsafe values fail
genesis generation. Missing fields use defaults only in the central resolver;
consumers cannot observe whether a value was explicit or defaulted.

## Production example

Omitting `outbeProtocol` selects and materializes the complete default profile.

```json
{
  "config": {
    "chainId": 54322346
  }
}
```

## LocalNet example

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
  -> outbe-chain constants genesis (immutable EE11 record)
  -> OCOMP bindings, keys, and genesis install
  -> TEE genesis policy
```

Changing any parameter changes the genesis hash. Existing networks cannot adopt
new values through an Update; a wipe and new genesis are required.
