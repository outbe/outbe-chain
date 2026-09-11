# Upstream provenance

- Project: `alloy-rs/alloy-evm`
- Package/release: `alloy-evm 0.38.0` (published crates.io source)
- Upstream source commit: `aa16b156595d7de8d25c31fc0c346e608fa7eb9c`.
- Local semantic delta: context-dispatch hook for Outbe stateful precompiles,
  ported from `outbedev/alloy-evm` commit
  `88e239e601eea130ef6644ffa672970e39332f70`.

The hook runs before ordinary precompile lookup and preserves the existing
raw-context-pointer API and its caller safety contract. No other upstream
execution behavior is changed.
