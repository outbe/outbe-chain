# Upstream provenance

- Project: `outbe/outbe-heartwood`
- Source: https://github.com/outbe/outbe-heartwood.git
- Commit: `b76a17801329291153585ed31db61ee3c658046e`
- Import: complete tracked source tree from `git archive`, including licenses,
  the standalone workspace and its lockfile; no nested Git repository.
- Local packaging delta: the Outbe sidecar library and its tests moved from
  `crates/outbe-radicle` into the chain's `crates/system/radicle` package
  (`outbe-radicle-runtime`). The duplicate entry point was removed in favor of
  the chain's existing `bin/outbe-radicle`. Runtime behavior is unchanged.
- Local build delta: the shared build script tracks enclosing Git refs and
  version environment overrides so build metadata refreshes after a chain commit
  or checkout, including linked worktrees.

Outbe workspace dependencies use paths into this tree. The E2E builder installs
`rad` and `git-remote-rad` from the local crate paths as well. Heartwood's build
script now obtains its Git build revision from the enclosing outbe-chain
checkout, so E2E verifies the binaries against that revision. Build manifests
also bind the source fingerprint and executable digests.
