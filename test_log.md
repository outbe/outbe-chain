# E2E expansion test log

This log records the evidence gathered while turning `docs/flows` into executable
end-to-end coverage. Commands are run from the repository root unless noted.

## 2026-07-17 — baseline and documentation contract

### Repository baseline

- Committed pre-existing localnet/E2E work as `1b499db` (`test(e2e): consolidate localnet scenarios in Rust harness`).
- Committed the ADR/PFS catalog as `f973076` (`docs(architecture): organize ADR and protocol flow catalogs`).
- `git pull --rebase`: PASS — branch was already up to date; no conflicts.
- Runtime-generated `data/` is intentionally untracked and excluded from commits.

### PFS format decision

- Reviewed the official Cucumber Gherkin reference and SEI quality-attribute
  scenario format.
- Adopted a combined `Acceptance contract`: Source, Trigger, Environment,
  Canonical inputs, System under test, Expected response, Response measures and
  Failure guarantee.
- Added that contract to PFS-001 through PFS-006 and `docs/flows/template.md`.
- Verification pending: Markdown/link/identifier checks after acceptance examples
  and automation mappings are complete.

### Initial automation inventory

- Live-node harness already contains Tribute projection/absence, Update lifecycle,
  validator lifecycle, restart, DKG failure, stale join and downtime scenarios.
- In-process suite already contains `wwd_lysis_nod_gratis.rs`,
  `governance_lifecycle.rs` and `update_flow_spec.rs`.
- Next: map every PFS matrix row to assertions actually present in those tests,
  then implement the highest-value uncovered vertical slice at the strongest
  feasible level.

### PFS-001 tracer bullet

- Added stable live-node tags `@pfs-001-01`, `@pfs-001-02` and `@pfs-001-03`
  to the already evidenced Tribute projection/proof scenarios.
- Added `@pfs-001-05`: submit a second encrypted offer for the same owner/day,
  require a reverted receipt, unchanged `totalSupply == 1`, and exactly one
  unchanged primary/owner/day projection on every validator.
- `cargo fmt --all -- --check`: initially RED — one rustfmt line wrap.
- `cargo fmt --all`: PASS — applied canonical formatting.
- `cargo test -p outbe-e2e-harness --no-run`: PASS — harness library and binary
  test targets compile.
- Live-node execution remains pending until the scenario-classification pass is
  complete, so related scenarios can be run together once rather than rebuilding
  the localnet repeatedly.
