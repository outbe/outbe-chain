# Protocol gas and latency benchmarks

The first benchmark release is intentionally Rust-only. It runs production Rust
handlers, codecs, storage meters, and portable in-process crypto that are callable
without starting a node. It does not invoke Foundry, compile or deploy Solidity,
load contract artifacts, or construct a full EVM world state.

Run the complete suite:

```bash
cargo xtask protocol-bench run --samples 30 --filter all
```

Write JSON or operate on the deterministic gas baseline:

```bash
cargo xtask protocol-bench run --samples 30 --filter tribute --json /tmp/bench.json
cargo xtask protocol-bench baseline-check --samples 3 --filter all
cargo xtask protocol-bench baseline-update --samples 3 --filter all
```

Latency is informational and excluded from the baseline. Gas, calldata, storage,
events, child frames, and fixture digests must be deterministic between samples.
`SystemVisible` and `SystemInternal` are independent ledgers and must never be
summed.

## Fidelity

- `FULL` means the complete named Rust seam executed.
- `PARTIAL / STUBBED` means a production boundary was deliberately excluded.
  Every excluded child is printed as `BenchmarkStub` with `gas_used = 0`; the
  benchmark never estimates unavailable gas.

The Credis smart-account boundary is tracked by `outbe-chain-6le.5`, Solidity
Gem/Intex child frames by `outbe-chain-6le.6`, and full system-transaction EVM
execution by `outbe-chain-6le.7`.

For system transactions, the Rust-only suite measures canonical V2 encode/decode
and `SystemTxVisibleGasPlan`. The report includes exact visible intrinsic gas and
TEE protocol precharge. `SystemInternal = 0` means “not measured in this release,”
not that production execution is free; the excluded executor frame and TODO make
that limitation explicit.
