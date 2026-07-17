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

### PFS-005/PFS-006 mapping and in-process edge case

- Added stable live-node tags for the evidenced Update, validator join/stale
  join/DKG/restart/downtime scenarios. Partial assertions remain marked partial in
  the PFS matrix (for example, ZeroFee downtime proves liveness but not slashing).
- Added in-process `duplicate_ballot_is_rejected_without_changing_vote_to_update_outcome`.
- Targeted duplicate-ballot test: PASS (1 passed).
- First full `cargo test -p outbe-e2e`: RED — 14 passed, 1 failed.
  `executor_runs_vote_before_update` omitted the mandatory compressed-entity
  genesis schema/root and failed before exercising hook order.
- Fixture correction: seed the protocol empty sealed root and schema slots before
  invoking the production pre-execution hook chain.
- Corrected `cargo test -p outbe-e2e`: PASS — 17 integration tests total
  (governance 1, Update 15, WWD/Lysis/Nod/Gratis 1).

### PFS-002/PFS-003/PFS-004 feasibility

- PFS-002 has an in-process cross-module WWD→Lysis→Nod→Gratis happy path, but no
  live finality/persistence/proof fixture.
- PFS-003 has an in-process pledge→Credis→10 repayments→reclaim path. Its proof
  verifier and Solidity vault/token subcalls are explicitly stubbed, so it is
  partial rather than production-interface coverage.
- PFS-004 has Rust runtime tests and Foundry bridge tests, but no fixture composes
  the Rust modules with deployed ERC-1155, vault and paired bridge adapters. Its
  full-flow scenarios remain documentation-only with per-row blockers.
- `cargo test -p outbe-credisfactory tests::e2e`: PASS — 8/8, including the
  ten-installment happy path and request/payment validation edges.

### Live-node Tribute verification

- Targeted `@pfs-001-05` on four validators with mock TEE and managed Mongo:
  PASS — 1 scenario, 6/6 steps.
- Full `tribute_projection.feature` on the same topology: PASS — 4/4 scenarios,
  19/19 steps.
- Harness removed both isolated run directories and managed containers after
  completion.
