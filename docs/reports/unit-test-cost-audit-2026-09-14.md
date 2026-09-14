# Unit test cost and duplication audit

Source: `main` at `fd16ab630e9fec48351bbcc09f4153cc4655d538`.
Change branch: `test/unit-capacity-e2e`.

The suite can be optimized, but deleting similar-looking assertions is not the
main opportunity shown by the available execution evidence. A few large-data
tests dominate runtime. Some actual duplicates also exist and should be cleaned
up for clarity and honest coverage claims.

## Scope and evidence limits

The inventory covers workspace Rust source under `crates`, `bin`, `testing` and
`xtask`. Cargo metadata reports 75 workspace packages and 216 integration-test
targets. A lexical scan found 4,939 test-function declarations under package
`src` directories and 1,116 under package `tests` directories. These are source
declarations, not an exact runnable-test count: feature gates, platform gates,
macros and child-process test invocations affect execution. Tests of the E2E
harness itself are included; Cucumber scenarios are not.

Graph discovery was attempted. Its recorded generation was September 4, with
excluded `bin` paths and stale or untracked split test files. Material findings
were therefore checked directly against current source after coverage queries.
The scan is a candidate inventory, not a semantic proof of redundancy across all
tests. No production change follows from this audit.

Runtime evidence comes from the existing September 13
[CI test job](https://github.com/outbe/outbe-chain/actions/runs/34747642001/job/103698399100)
at `a0782b700c4ccb90b1ff5015dc7d597ada0aabd9`, under LLVM coverage and an
unoptimized test profile. That job failed on the previously diagnosed xtask Git
ownership check; it is not a passing validation of current main. The three
capacity files, reconstruction helpers, cleanup fault test and artifact test
file discussed below were unchanged between that revision and the audited main.

Cargo reported 5m18s of build time. Execution ran from approximately 10:34:04 to
11:00:41 UTC, about 26m37s. This is a cached CI observation, not a clean-build
benchmark. Log stdout/stderr ordering and nested child-test summaries prevent
using a naive sum of all result lines as an exact count of independent tests.

## Measured runtime concentration

Times below are for whole test binaries, not isolated individual tests. Each of
the first three binaries has five tests, and its large-data test triggered the
libtest warning for running longer than 60 seconds.

| Test binary | CI runtime | Main large-data operation |
| --- | ---: | --- |
| `outbe-ocomp / streaming_input_artifacts` | 275.55 s | Publish 1,000,000 records through the durable publisher; check a 256-record peak |
| `outbe-compressed-entities / bounded_collection_reconstruction` | 180.65 s | Compare bounded and eager roots at populations through 4,097 |
| `outbe-ocomp / input_inventory` | 111.26 s | Build, sort, stream and reopen an inventory of 4,097 bodies |
| `outbe-lysis` library | 107.41 s | Includes dense-day Lysis execution |
| `outbe-evm` library | 88.10 s | Includes worst-case active TEE expiry sweep |
| `outbe-ocomp / deterministic_schedules` | 82.40 s | Compare actual 257-Tribute computation across 1/2/4-worker schedules |
| `outbe-compressed-entities` library | 82.18 s | Includes rollback at every cleanup mutation boundary |
| `outbe-e2e-harness` library | 75.63 s | Includes artifact identity and tampering checks |

The first three sum to 567.46 seconds, about 35.5% of the observed execution
interval. Moving their three heavy cases out of the default run should remove a
substantial part of that cost. It does not remove compilation of their test
binaries or execution of the other twelve tests. Therefore 9m27s is the measured
cost of the three whole binaries, not a guaranteed before/after saving.

## Verified duplication and misleading test names

| Location | Finding | Bounded follow-up |
| --- | --- | --- |
| `crates/system/zerofee/src/runtime.rs`, `authorize_accepts_zero_balance_signer` and `authorize_accepts_existing_account_with_balance_only` | Identical fresh storage, signer, call and assertions. Neither creates a funded account. The adjacent balance-independence test also compares two calls without changing a balance. | Keep one policy-level acceptance test. Check real zero/funded account behavior at the layer that actually supplies account state. |
| Same file, the two `precheck_accepts_*_non_paymaster_signer` tests | Identical call to `precheck_sponsorship(SIGNER)`. The quota-named test repeats that call too. | One ordinary-signer acceptance assertion; do not claim this sets up a funded account or exhausted quota. |
| `crates/blockchain/txpool/src/lib.rs`, `pool_precheck_admits_*` | Same imported ZeroFee helper and same signer for zero/funded cases. These calls do not invoke pool admission. Several adjacent `pool_classify_*` cases directly retest the imported policy helper. | Keep policy truth tables in ZeroFee and retain tests that actually exercise pool admission/wiring. Review those wiring tests before deleting helper-only cases. |
| `bin/outbe-cli/src/main.rs` and `commands/vote.rs`, `test_cli_parse_vote_status` | Same root `Cli`, same argv, same `is_ok()` assertion. | Keep one copy. |
| `crates/blockchain/consensus/src/dkg_actor/actor/tests/bootstrap.rs`, `test_bootstrap_dkg_waits_for_all_genesis_nodes_one_offline` and `test_bootstrap_dkg_does_not_return_threshold_subset_output` | Both run `run_partial_dkg(&context, 4, 3)` with the same deterministic runner and five-second virtual non-completion window; only explanations/panic text differ. | Keep one case with both motivations documented. Preserve the distinct 7/5 and 4/2 populations. |

Whitespace-normalized bodies yielded 11 candidate pairs, not 11 proven redundant
tests. For example, Tribute and Nod repository `memory_contract` wrappers look
identical but call different module-local contract helpers over different entity
types. Removing one would lose coverage. Likewise, memory and RocksDB backends,
unit policy and actual executor wiring, and distinct corruption boundaries are
not interchangeable merely because assertions look similar.

The entire ZeroFee library took 0.03 seconds in the sampled CI job. Its redundant
assertions matter for maintainability and accuracy of test names, not minutes of
runtime. No duplicate cleanup was implemented in this change.

## Other concrete optimization candidates

1. **Avoid recomputing the same expected root.** In
   `bounded_collection_reconstruction.rs`, `verifier()` computes
   `expected_root(day, leaves)` for the expectation; the boundary test computes
   it again for the final assertion. Compute that immutable expected value once
   per population and reuse it. Preserve the independent eager-versus-bounded
   comparison, all boundary populations and shuffled input. Savings require a
   before/after measurement; the whole binary's 180.65 seconds is not all
   redundant work.

2. **Avoid repeatedly hashing the harness executable in artifact unit fixtures.**
   `artifacts.rs::manifest_fixture()` calls `required_artifacts()`, which includes
   `current_exe()`. `identify()` calls `hash_file()`, which reads and hashes the
   complete file. The sidecar-negative test rebuilds this fixture six times and
   then calls preflight. A small fixture artifact at an existing test seam could
   avoid unrelated executable hashing while retaining a real current-executable
   integration check. Do not cache production artifact validation: changed bytes,
   permissions, paths and symlinks must continue to be detected. The 75.63 seconds
   is whole-library time; no precise savings have been measured.

3. **Separate expensive fixture construction from fault enumeration where safe.**
   The cleanup rollback test rebuilds and mints its fixture for each before/after
   mutation fault. Investigate cloning a fully isolated prepared test state,
   including event and scope state, rather than repeating unrelated setup. Keep
   every fault position, complete state/event equality and successful retry.
   Shared mutable providers or dropping fault positions would weaken the test.

4. **Measure before changing scheduling or test profiles.** There are many
   separate integration binaries and the current test profile only sets
   `debug = 0`. Compilation, linking, coverage instrumentation and execution are
   different costs. This audit does not justify globally enabling optimization,
   merging every integration file or increasing concurrency. Also, source
   `sleep()` counts are not wall-time measurements: the DKG examples above already
   use Commonware virtual time.

## Approved change: run the three capacity cases with E2E

The three heavy functions are marked with an explicit `#[ignore]` reason. Their
implementations, data populations and assertions remain intact, and the other
twelve tests in those files stay enabled in ordinary Cargo/nextest runs.

`mise run e2e-capacity` selects those exact three ignored tests with release
builds. It is a dependency of the existing full `mise run e2e` and
`mise run e2e-sgx` tasks, so a capacity failure fails the E2E task before scenario
execution. It does not enable unrelated ignored MongoDB or hardware tests.

```sh
# The full SGX E2E task includes the capacity checks automatically.
mise run e2e-sgx

# Run only the capacity portion of E2E.
mise run e2e-capacity
```

Calling the `outbe-e2e` binary directly bypasses mise task dependencies; for a
manual full run, execute `mise run e2e-capacity` as well. The focused storage,
Radicle and DCAP tasks are not redefined as full-suite acceptance tasks.

This change moves cost to E2E; it does not claim to make the same checks cheaper
or remove the need to run them before release.

## Validation of the change

- Ordinary release runs of the three changed test files: **12 passed, 3 ignored**.
- The final E2E capacity command, executed with Rust 1.96.0, offline dependency
  resolution and four CPUs: **3 passed**, with exactly one selected test and four
  filtered tests in each binary. Reconstruction took 11.18 s, inventory 12.53 s,
  and the million-record publisher 18.41 s.
- The task uses one Cargo invocation for both packages and all three targets,
  avoiding separate invocations with different dependency-feature selections.
- `cargo fmt --all -- --check`, Git whitespace checks, TOML parsing and mise
  dry-runs of both full E2E task dependency chains passed.

These local release timings are not directly comparable with the historical
unoptimized coverage timings: the build profile, instrumentation and machine
differ. No full workspace, Cucumber or hardware E2E suite was executed for this
test-selection change. Local diagnostic logs are under
`/tmp/unit-audit-20260914/`.
