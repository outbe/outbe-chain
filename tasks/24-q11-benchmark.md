# T24 — Q11 worst-case benchmark harness and numerical closure

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §15.2, §8.3 (Q11 open),
        `compressed_entities_v6_performance_benchmark_requirements_10-07-2026.md` (normative requirements)
Depends on: T10, T12, T16, T18, T23 (benchmark workload needs the registered Tribute/Nod schemas,
max body sizes, and registry entries whose K_domain it finalizes), T30 + T31 (Part B re-baselines their
provisional values — audit-final L-01), T34 (approved hardware profile + benchmark protocol precede any
candidate run — audit-final B-06/H-06)
Blocks: T14 (Part B Stage 1 testnet genesis re-baseline); final constant activation

## Summary

Build the reproducible worst-case benchmark required to close Q11 and produce the final numerical limits
(gas formula, per-tx/per-block caps, byte limits, staged-tree bounds), replacing the provisional guard values.

## Context

Selection is iterative: choose candidate limits → construct the saturated worst case at exactly those
limits → measure → accept or reduce until the target holds with margin. Workload requirements: every entity
mutation a new key; all mutations in one collection shard; collection retirement + Root Catalog updates
included; bodies at maximum size; Poseidon hashing, leaf construction, SMT node hashing, top-root, journal
cleanup included; `OnStateHook(PostBlock(Other(...)))` notification included; proof reads and MDBX
persistence concurrent; minimum supported validator hardware. Target: gas-saturated
`full_block_execution_time < 2 s` under the default consensus timing contract.

## Structure (audit P1-0b): two parts

- **Part A — benchmark harness + candidate search**: the reproducible workload, parameterization,
  measurement runs, candidate-limit iteration.
- **Part B — split into ordered sub-parts (postfix PF-B03; the former single node required green
  downstream consumers whose own Part-B work starts only after this task — an unexecutable completion
  order):**
  - **B1 — constants publication**: merge final values and regenerate the SHARED golden fixtures T24
    owns; T24's completion gate. Consumers: T02, T04, T12, T14, T15, T17, T18, T20, T21, T22, T23.
  - **B2 — downstream re-baseline**: each consumer suite re-runs green ON its own task (T14's Stage 1
    testnet genesis re-baseline executes here, AFTER B1 publishes constants); T24 keeps the tracking
    checklist but does not gate on the re-runs.
  - **B3 — evidence join**: T25 asserts every B2 re-run is green before release sign-off.

Pass metric (decided): the acceptance gate is EXECUTION-ONLY `full_block_execution_time < 2 s` (the
§8.3/§15.2 wording); the end-to-end path through durable Reth persistence + MDBX commit + Marshal ACK is
measured and REPORTED alongside with its own budget note, but does not gate the 2 s number (the
benchmark-requirements doc measures both). Minimum validator hardware is an INPUT (fixed profile);
the numeric safety margin is an OUTPUT of the report.

The minimum validator hardware profile is a VERSIONED, APPROVED T34 artifact (audit-final B-06): the
harness verifies the benchmark host against the profile ID and FAILS CLOSED on mismatch. The benchmark
protocol — numeric target scale, concurrency, cold-cache procedure, repetition count, the gating
statistic, outlier policy, and minimum safety margin — is likewise fixed in T34 BEFORE candidate runs
(audit-final H-06); a run under a different protocol version is invalid evidence.

## Scope

- Criterion-based (or dedicated binary) harness generating the §15.2 workload deterministically from a seed;
  candidate-limit parameterization; concurrent proof-read + MDBX-commit load; a READ-PATH benchmark
  component measuring Mongo point-read/partition fetch plus per-body leaf verification under the workload
  (report metrics — reads carry no protocol bounds after the 2026-07-13 re-cut).
- Required outputs: `max_unique_keys_per_block`, `max_ce_mutation_attempts_per_tx`,
  `max_ce_mutation_attempts_per_block`, aggregate body/calldata/event byte limits, deferred-seal gas charge
  per operation/byte/key, §13.1 staged-batch retention bounds (node-local cache), the MEASURED worst-case
  staged-batch size and heap peak at the final attempt caps (REPORT metrics proving the memory/2 s budget —
  owner decision 2026-07-13: no protocol byte limit), and the final
  per-domain `collection_shard_count` for Tribute/Nod (§9.1: selected by Q11; frozen into the genesis
  commitment scheme — changing later requires migration + new scheme, so this selection is a benchmark
  deliverable, not a T23 default; selected per the T34 protocol's versioned candidate set, objective
  function, and deterministic tie-break — postfix PF-M06).
- Companion micro-benchmarks (§19.14): worst-case single-collection/single-shard throughput,
  multi-collection parallel preparation, MDBX growth, partition-retirement namespace reclamation, journal
  cleanup, `SealOutput` handoff/drop, state-root notification, concurrent proof-serving/tree-commit.
- Activation-evidence report: hardware profile, dataset shape, cache state, commands, raw results, safety
  margin — reproducible from the repo.
- Final constants PR: replace provisional values, recalibrate lane behavior, update spec §15.1/§20 and README.
- Final-limit consumer matrix (audit-final H-01, narrowed after the re-cut removed the recovery
  archive): Part B publishes the full consumer matrix — T07 body limits, RPC/snapshot resource bounds —
  nothing is left on stale provisional math.
- Schema-compatibility proof (audit-final M-05): a domain × operation × schema matrix proving
  `max_schema_encoded_size <= final_limit` for every Tribute/Nod operation, or an intentional
  schema/input-bound reduction with a full re-baseline.

## Out of scope

- Changing the 2 s timing contract or validator hardware floor (inputs, not outputs).

## Acceptance criteria

1. One-command reproduction (`mise run` target) producing the full report on the reference hardware
   profile — this report is the §19.13 release-gate artifact.
2. All §15.2 outputs emitted; acceptance target met with documented safety margin, or candidates reduced and
   re-run (loop captured in the report).
3. Re-run triggers documented: hardware floor, gas limit, hash implementation, SMT codec, or persistence
   path changes invalidate the report.
4. Final constants merged with README/spec updates; provisional markers removed (Q11 closure). Per-domain
   `K_domain` values registered in T23's registry entries before genesis. Because `K_domain` changes
   `shard_index` extraction and collection-top depth, all affected golden vectors (identity/roots/proofs/
   genesis/snapshots) are REGENERATED at the final values (Part B1); the FULL affected set — T02, T04,
   T12, T14, T15, T17, T18, T20, T21, T22, T23 suites plus end-to-end — re-runs green as B2 deliverables
   owned by the consumer tasks, tracked by T24's checklist and joined by T25 (postfix PF-B03); T14's
   genesis re-baseline is a B2 consumer, not a B1 gate.
5. Final limit structure ENFORCED, not only merged: T24 IMPLEMENTS the concrete key/byte/staged-tree
   counters over T10's limit-kind interface once the benchmark fixes their structure and values, with
   proposer/validator rejection tests at each final limit.
6. Host/profile gate (audit-final B-06): the harness refuses to produce a report on a host that does not
   match the approved T34 profile ID; the report embeds the profile ID and protocol version.
7. Re-baseline artifacts (audit-final H-01/M-05): the recovery-capacity formula, the final-limit consumer
   matrix, and the schema-maxima matrix are included in the Part B PR.

## Invariants

- Benchmark code never ships in consensus paths; constants land as `const` protocol values.

## Tests

- Harness determinism check (same seed → same workload); smoke-scale CI variant (not the full saturation run).

## Files

- `crates/core/compressed_entities/benches/`, `mise` task, activation report under `outbe-plan/` or `docs/`
