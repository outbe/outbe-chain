# Upstream provenance

- Project: `paradigmxyz/reth`
- Package: `reth-transaction-pool`
- Release: `v2.5.2`
- Source commit: `5a6940e351fed80458fe6c9da8581cbe4b8bd036`
- Local semantic delta: queued-lifetime maintenance emits one structured
  `outbe::txpool` warning for each transaction actually removed, including its
  hash, sender, nonce, and `queued_lifetime` reason.

The eviction filter, deadline, removal operation and consensus behavior are
unchanged. Workspace manifest inheritance is expanded for standalone vendoring;
the self dev-dependency resolves to this local package.
