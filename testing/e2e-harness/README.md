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
| `PFS-002-01` through `-24` | OCOMP implementation and focused production-adapter tests exist; `-07`/`-08` remain deferred and exact-revision Linux four-domain closure evidence is pending |
| `PFS-005-01`, `-09` plus named recovery/rejection tags | Vote approval/activation, restart boundaries, rejection paths, unsupported-version stall and operator binary replacement |
| `PFS-006-01`, `-02`, `-03`, `-04`, `-06`, `-09` | Join/exit/claim accounting, stale join, DKG recovery, slash idempotency, checkpoint restarts and full-committee sealed TEE recovery |
| `PFS-007-01` through `-12` | Pectra/ZeroFee readiness, native EIP-7702 delegation, quota/fallback, exact replay, restart persistence, invalid authorization and day reset |
| `PFS-008-01` through `-08` | Cold/chained sync, upstream loss/switch, validator recovery, boundary restarts and idempotent warm promotion |
| `PFS-010-01` through `-04` | Shared policy, bonded Factory approval/refund, issuer ledger operations, duplicate-ticker rejection and same-binary full-committee restart |

Run one mapped example with `--tags '@pfs-001-05'`. A tag means that the
scenario supplies the evidence stated in its PFS matrix row; it does not imply
coverage of assertions that the row explicitly marks as a gap.

## Layout

- `features/` — Gherkin fixtures. `update_operator.feature` is wired end-to-end;
  `tribute_projection.feature` covers encrypted-offer projection plus compressed
  entity presence and absence proofs; `stablecoin_factory_v1.feature` covers the
  fresh-genesis Factory product flow and full-committee restart.
  `ocomp_public_path.feature` includes the Linux-only fresh Metadosis path:
  finalized runtime block-1 `Create`, two whole-committee logical-time restart
  barriers with canonical phase durations, and terminal OCOMP on the same WWD.
- `release-features/` — the separate exact-artifact hardware-SGX acceptance scenario.
  It does not bootstrap a localnet or MongoDB and never rebuilds the release image.
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
- `--tee <real|gramine-direct|mock>` — mandatory enclave mode (default `mock`);
  `gramine-direct` uses the production enclave binary without SGX. Both non-SGX
  modes run only on the isolated `GramineDirectDev` chain and are not hardware
  evidence.
- `--no-sudo` — run scripts/docker without `sudo`.
- `--all` — treat an unsatisfiable scenario as a failure instead of skipping it.
- `--debug` — stream localnet setup output (bootstrap / run-testnet / docker) live;
  off by default (that output is captured and shown only if a step fails).
- `--projection-mongodb-uri <URI>` — optional transaction-capable MongoDB replica set or sharded
  cluster. When omitted, the harness starts and owns a temporary `mongo:7.0`
  single-node replica set. Either way each node gets a distinct logical database.
- path overrides (optional, default relative to `--repo`): `--repo`, `--data-dir`,
  `--chain-bin`, `--cli-bin`, `--keygen-bin`, `--enclave-bin`, `--mock-bin`,
  `--seed`.
- `--evidence-dir <PATH>` — persistent per-scenario JSON evidence. By default it
  is written under `<data-dir>/evidence/<run-id>` and is not removed when a
  successful run cleans its node data.
- `--metadosis-p0-case <debug-unset|debug-named|debug-arbitrary|release-unset|release-named|release-arbitrary>`
  — test-only binding used by the Metadosis P0 parity runner. It validates and
  retains the removed process input inherited by every node child; ordinary
  harness runs leave it unset.
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
cargo build --release -p outbe-tee-enclave --bin outbe-tee-enclave
cargo build --release -p outbe-tee-enclave --features mock --bin outbe-tee-enclave-mock
```

Then, e.g.:

```sh
# Omit --projection-mongodb-uri to use the harness-owned replica set.
# Through the mock enclave on an isolated GramineDirectDev chain.
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock --validators 4
# a fully-capable box: everything must run (unmet ⇒ fail, not skip)
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock --validators 4 --all
```

The Metadosis P0 env-independence closure is a separate Ubuntu 24.04 x86_64
lane. It requires a clean checkout and a new output path outside the repository:

```sh
cargo run --locked -p outbe-e2e-harness --features ocomp-integration \
  --bin outbe-metadosis-evidence -- \
  p0-parity --output /path/to/new-evidence-directory
```

The runner builds normal debug and release node artifacts without
`outbe-metadosis/test-utils`, executes the existing canonical 257-owner Final
scenario for all six env/profile cases, and publishes a fail-closed comparison
of receipt, state-root, CE-root, and four-validator import commitments. Use the
same Docker/Gramine prerequisites as the harness's existing OCOMP public-path
scenarios.

The fresh-devnet lifecycle lane is also owned by the same Rust harness binary:

```sh
cargo run --locked -p outbe-e2e-harness --features ocomp-integration \
  --bin outbe-metadosis-evidence -- \
  fresh-devnet --output /path/to/new-fresh-devnet-evidence
```

The complete Metadosis pack is owned by `outbe-metadosis-evidence`. Its portable
host command runs on macOS or Linux, requires the pinned Linux/amd64 image to be
present locally, and writes to a new bundle path outside the repository:

```sh
cargo run --locked -p outbe-e2e-harness --features ocomp-integration \
  --bin outbe-metadosis-evidence -- \
  --repo . container-run \
  --output /tmp/metadosis-run \
  --image 'registry.example/outbe-evidence@sha256:<64-hex-digest>'
```

The host side only creates and inspects Docker containers. It never substitutes
a native result for the Linux lane. The current Docker context must expose a
local Unix socket; TCP/SSH contexts are rejected. The pinned image must include
the Docker client, and the selected macOS/Linux container runtime must support
host networking (when using Docker Desktop, enable that feature explicitly).
The launcher mounts that exact socket plus repository,
evidence and temporary target paths at the same absolute paths, runs as the
numeric host UID/GID, sets the inner daemon endpoint explicitly, and records
the topology in provenance. The first pinned container independently
lists normal and ignored Cargo tests, executes every fixed command once, and
invokes the harness-owned six-case P0 and fresh-devnet process subcommands. The
launcher retains raw image/container inspection documents and binds their exact
image ID, container ID, command, digest, and one-time nonce to the runner
receipt. A second container from the same digest performs `assemble` and
`verify`: status is derived from discovery and exit facts, the manifest is
reconstructed, and all retained receipts, logs, binaries, scenarios, and Docker
facts are rehashed. The lower-level `run`, `assemble`, `verify`, and `finalize`
subcommands are internal lane stages, not alternative host evidence contracts.

Repository evidence catalogs are indexed by
`outbe-plan/verification-ledger.yaml`. The generic verifier keeps OCOMP and
Metadosis domain policies separate, rejects duplicate/cross-pack references and
requires one exact source/toolchain/artifact profile. Before accepting a
Metadosis manifest, the domain-neutral CLI invokes the Metadosis adapter to
reconstruct it from `runner-receipt.json`; a caller-authored PASS cannot bypass
that derivation. Run
`outbe-verification-ledger validate-index` to load both packs strictly. Its
`verify` command takes paired `--evidence` and `--bundle-root` arguments and
binds the current checkout, immutable Linux image digest, exact test
target/name/lane/substitutions and every referenced member byte. The checked-in
Metadosis pack declares required production-interface tests; it is not a claim
that the external Linux lane has already run.

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
  --tee mock --validators 4 --all \
  --input testing/e2e-harness/features/zerofee.feature
```

It is also part of the canonical `mise run e2e` suite. The Rust World owns its
network, transaction signing, receipts and cleanup; Foundry `cast` is not used.

The skip/fail *logic* is verifiable anywhere (no localnet needed): e.g.
`--validators 2` prints `SKIPPED: … needs >=4 validators, have 2` and exits 0,
while `--validators 2 --all` exits non-zero.

`--debug` streams the localnet setup output live; without it, that output is
captured and only printed if a setup step fails.

## Published testnet SGX release acceptance

`outbe-release-sgx-e2e` is a second Rust/Cucumber entrypoint for one exact published
`image@sha256:...`. It is separate from the localnet World because release acceptance owns
an immutable bundle/image/sealed-state fixture, not validators or MongoDB:

```bash
cargo run -p outbe-e2e-harness --bin outbe-release-sgx-e2e -- \
  --image 'ghcr.io/outbe/outbe-tee-enclave-testnet@sha256:<64-hex-digest>' \
  --bundle /tmp/extracted-signed-sgx-bundle \
  --evidence /tmp/hardware-sgx.json
```

It requires real SGX devices and a Docker-pulled, Cosign-verified digest. The scenario:

1. reruns typed bundle verification and checks the runtime has no key generation, signing
   or direct-mode fallback;
2. obtains a local SGX report from the running image and compares its
   MRENCLAVE/MRSIGNER/ISVPRODID/ISVSVN with the signed bundle;
3. requires both MRSIGNER- and MRENCLAVE-policy EGETKEY access;
4. starts the same image twice with one sealed directory and requires same-signer identity
   restoration;
5. changes one signed artifact and requires verification failure; and
6. re-signs a test copy with an ephemeral different key and proves the old MRSIGNER-sealed
   identity is rejected rather than silently restored.

Only after every step passes does it write canonical `hardware-sgx.json`. DCAP is recorded
as unavailable/false under the current `remote_attestation = "none"` release contract; the
scenario must not be cited as remote-attestation evidence.

## Focused Tribute compressed-entity checks

Run the complete Tribute compressed-entity feature (happy path and edge cases):

```sh
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee gramine-direct \
  --validators 4 \
  --input 'testing/e2e-harness/features/tribute_projection.feature'
```

Run only the creation happy path:

```sh
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee gramine-direct \
  --validators 4 \
  --name "A successful tribute is persisted by every validator"
```

The scenario performs the complete product flow:

1. Starts an isolated four-validator localnet and the production enclave binary
   under `gramine-direct`.
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
