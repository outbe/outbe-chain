# Upstream provenance

- Project: `alloy-rs/alloy-evm`
- Package/release: `alloy-evm 0.39.0` (published crates.io source)
- Upstream source commit: `ba6f83b80aba8cf005175f4d776d8b90796c72d9`.
- Local semantic delta: context-dispatch hook for Outbe stateful precompiles,
  ported from `outbedev/alloy-evm` commit
  `88e239e601eea130ef6644ffa672970e39332f70`.

The hook runs before ordinary precompile lookup and preserves the existing
raw-context-pointer API and its caller safety contract. No other upstream
execution behavior is changed.
