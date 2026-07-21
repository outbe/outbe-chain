# outbe-e2e-harness

A Rust [cucumber](https://crates.io/crates/cucumber) harness for the outbe-chain
e2e suite. Scenarios are Gherkin fixtures under [`features/`](./features); the
step code behind them (`src/features/`) drives typed handles (`src/world/`).

The harness owns validator processes, docker/Gramine TEE enclaves, and optional
MongoDB containers. DKG bootstrap and genesis seeding remain one-shot
subprocesses.

## Model: environment (CLI) vs. requirements (tags)

The **CLI defines the environment** — how many validators to bootstrap, the TEE
mode, and whether we have `sudo`. Each **scenario declares its requirements** via
Gherkin tags. The runner matches the two:

- requirement met → the scenario runs;
- requirement unmet → the scenario is **skipped** (a `SKIPPED:` line prints, exit 0);
- with `--all`, an unmet scenario is a **failure** instead (non-zero exit).

Requirement tags (`@`-less in code): `tee`, `min-validators-N`, `sudo`,
and `todo` (an unimplemented stub — always skipped).

Traceability tags use stable scenario ids from `docs/flows`, for example
`@pfs-001-05`. They do not alter environment selection and can be passed directly
to Cucumber's `--tags` filter. Current live-node mappings are:

| PFS examples | Feature coverage |
|---|---|
| `PFS-001-01`, `-02`, `-03`, `-05` | Tribute creation/projection/proof, two absence scopes and duplicate logical offer rejection |
| `PFS-005-01`, `-09` plus named recovery/rejection tags | Vote approval/activation, restart boundaries, rejection paths, unsupported-version stall and operator binary replacement |
| `PFS-006-01`, `-02`, `-03`, `-04`, `-06`, `-09` | Join/exit/claim accounting, stale join, DKG recovery, slash idempotency, checkpoint restarts and full-committee sealed TEE recovery |
| `PFS-007-01` through `-12` | Pectra/ZeroFee readiness, native EIP-7702 delegation, quota/fallback, exact replay, restart persistence, invalid authorization and day reset |
| `PFS-008-01` through `-08` | Cold/chained sync, upstream loss/switch, validator recovery, boundary restarts and idempotent warm promotion |

Run one mapped example with `--tags '@pfs-001-05'`. A tag means that the
scenario supplies the evidence stated in its PFS matrix row; it does not imply
coverage of assertions that the row explicitly marks as a gap.

## Layout

- `features/` — Gherkin fixtures. `update_operator.feature` is wired end-to-end;
  `tribute_projection.feature` covers encrypted-offer projection plus compressed
  entity presence and absence proofs.
- `src/env.rs` — `TeeMode`, the `EnvCli` clap flags, `Environment`, and the
  requirement/skip logic.
- `src/world/` — encapsulated handles with verb APIs: `localnet.start(opts)`,
  `rpc.send_propose(...)`, `rpc.wait_block(...)`, `validators.operator(...)`.
- `src/features/` — step definitions (the code behind the fixtures).
- `src/internal/` — private plumbing: `Config`, the `xshell` wrapper, precompile
  addresses, output parsers.

## Running

The entrypoint is the `outbe-e2e` binary. **All configuration is via CLI flags —
the harness reads no configuration from the environment.** Flags:

- `--validators <N>` — committee size to bootstrap (default 4).
- `--tee <real|mock|none>` — enclave mode (default `none` = tee-less).
- `--no-sudo` — run scripts/docker without `sudo`.
- `--all` — treat an unsatisfiable scenario as a failure instead of skipping it.
- `--debug` — stream localnet setup output (bootstrap / run-testnet / docker) live;
  off by default (that output is captured and shown only if a step fails).
- `--projection-mongodb-uri <URI>` — optional transaction-capable MongoDB replica set or sharded
  cluster. When omitted, the harness starts and owns a temporary `mongo:7.0`
  single-node replica set. Either way each node gets a distinct logical database.
- path overrides (optional, default relative to `--repo`): `--repo`, `--data-dir`,
  `--chain-bin`, `--cli-bin`, `--keygen-bin`, `--mock-bin`, `--seed`.
- `--evidence-dir <PATH>` — persistent per-scenario JSON evidence. By default it
  is written under `<data-dir>/evidence/<run-id>` and is not removed when a
  successful run cleans its node data.
- `--upgraded-chain-bin <PATH>` — optional prebuilt replacement node binary for
  the protocol-update recovery scenario. When omitted, that scenario creates a
  temporary detached worktree at the revision under test, changes only its
  workspace package version, builds the requested binary offline, and removes
  the worktree after the build.
- plus cucumber's own `--tags`, `--name`, `--input`.

Actually executing a scenario needs a Linux box with `sudo` + `docker` + `gramine`
(same prerequisites as `mise run e2e`). First build the binaries the steps call:

```sh
cargo build -p outbe-chain --bin outbe-chain
cargo build --bin outbe-cli
cargo build --release -p outbe-tee-enclave --features mock --bin outbe-tee-enclave-mock
```

Then, e.g.:

```sh
# Omit --projection-mongodb-uri to use the harness-owned replica set.
# tee-less run of the update flow
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee none --validators 4
# through the mock enclave
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock --validators 4
# a fully-capable box: everything must run (unmet ⇒ fail, not skip)
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock --validators 4 --all
```

The same full run is available as `mise run e2e`. The harness owns an isolated
MongoDB replica set unless `--projection-mongodb-uri` is supplied explicitly.
On an SGX runner, `mise run e2e-sgx` builds the real enclave and runs the same
features with four `gramine-sgx` containers. That lane raises the per-request
TEE timeout to 120 seconds for EPC paging while retaining the normal 30-second
default elsewhere. It also passes a 180-second node-local TEE bootstrap deadline
and polls for up to 240 seconds outside that deadline, because four co-located
hardware enclaves have exceeded both the node's normal 60-second bootstrap
default and the host client's normal 30-second request deadline in consecutive-run
evidence. The production/testnet defaults remain unchanged and must be calibrated
for their deployment topology.

Run only ZeroFee's native Alloy EIP-7702 set-code and sponsorship vertical slice:

```sh
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee none --validators 4 --all \
  --input crates/testing/e2e-harness/features/zerofee.feature
```

It is also part of the canonical `mise run e2e` suite. The Rust World owns its
network, transaction signing, receipts and cleanup; Foundry `cast` is not used.

The skip/fail *logic* is verifiable anywhere (no localnet needed): e.g.
`--validators 2` prints `SKIPPED: … needs >=4 validators, have 2` and exits 0,
while `--validators 2 --all` exits non-zero.

`--debug` streams the localnet setup output live; without it, that output is
captured and only printed if a setup step fails.

## Focused Tribute compressed-entity checks

Run the complete Tribute compressed-entity feature (happy path and edge cases):

```sh
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock \
  --validators 4 \
  --input 'crates/testing/e2e-harness/features/tribute_projection.feature'
```

Run only the creation happy path:

```sh
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock \
  --validators 4 \
  --name "A successful tribute is persisted by every validator"
```

The scenario performs the complete product flow:

1. Starts an isolated four-validator localnet and mock TEE enclaves.
2. Starts a temporary `mongo:7.0` single-node replica set. Pass
   `--projection-mongodb-uri <URI>` to use an existing transaction-capable
   deployment instead.
3. Submits one encrypted `offerTribute` transaction through `outbe-cli`.
4. Requires a successful receipt and `totalSupply == 1`.
5. Finds the primary document by `_projection.tx_hash`, derives its exact owner
   and Worldwide-Day index keys from the canonical body, and requires all three
   documents on every validator. The check does not assume the database contains
   only one Tribute, so lifecycle scenarios can validate later offers too.
6. Requires the exact primary/owner/day BSON documents to be identical across
   all four validators.
7. Calls `outbe_getCompressedEntity` on every validator, fetches the exact
   selected block header, and verifies each proof package independently.
8. Requires every validator's authenticated `Present` body bytes to equal the
   canonical bytes stored in MongoDB. Proof packages may select different
   finalized headers while validators converge, so each package is verified
   independently rather than compared byte-for-byte.

The edge-case scenarios independently verify both authenticated absence forms:

- `EntityAbsentInCollection` for an unknown Tribute identity in a day whose
  collection already exists;
- `CollectionAbsent` for an unknown Tribute day, while also asserting that no
  primary or secondary MongoDB projection was created.

The duplicate-identity scenario submits a second encrypted offer from the same
owner in the same Worldwide Day with a different amount and opposite Intex
exclusion flag. It requires a reverted receipt, unchanged supply, byte-identical
primary/owner/day Mongo documents, and exactly the original Tribute ID in both
on-chain indexes on every validator. Structured `E2E_TRIBUTE_TIMELINE` records
correlate submission, receipt block/events, canonical state, finality, and Mongo
visibility. Real-SGX runs widen only their scenario genesis consensus windows
because four enclaves share one host; production/testnet timing defaults remain
unchanged and must be calibrated on the deployment topology separately.

On normal completion or failure, the harness stops the nodes and removes its
MongoDB and TEE containers. SIGINT/SIGTERM also runs the managed-container
cleanup backstop. Add `--no-cleanup` when a successful run's chain data should
remain available for inspection; failed runs keep their data directory by
default.

Every scenario that constructs a World writes `scenario-NNN.json` before
teardown. The record includes the source SHA and dirty-worktree bit, exact
invocation, feature/scenario/result, duration, validator and TEE configuration,
scenario data directory, and explicit log-audit counts (including zeros). This
is compact durable evidence; verbose node logs remain in the run directory only
for failed runs or when `--no-cleanup` is used.

## Status

The focused `tribute_projection` scenarios own MongoDB and verify the full
encrypted offer → successful receipt → four-validator projection → independently
verified compressed-entity proof path, including both absence-proof edge cases. The
validator lifecycle, update, DKG, downtime, restart, stale-join, and follower
flows are also wired under `features/`. DKG failure coverage includes both recovery
of a stalled frozen target and permanent loss: the latter asserts that the outgoing
committee finalizes without partial activation through the published VRF deadline
and that every surviving validator then terminates fail-closed. It deliberately
does not claim an automatic forfeiture/replacement policy.

## Ide support

Cucumber framework provides support for VSCode.
Add extension "cucumberopen.cucumber-official", and set the following "Glue" in settings:

```json
{
    "cucumber.glues": [
        "**/src/features/**/*.rs", // To support any crate with cucumber framework.
        // ..
    ]
}
```
