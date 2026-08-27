# outbe-e2e-harness

A Rust [cucumber](https://crates.io/crates/cucumber) harness for the outbe-chain
e2e suite. Scenarios are Gherkin fixtures under [`features/`](./features); the
step code behind them (`src/features/`) drives typed handles (`src/world/`).

The harness owns validator processes, docker/Gramine TEE enclaves, and optional
MongoDB containers. DKG bootstrap and genesis seeding remain one-shot
subprocesses.

## Persistent dev LocalNet

The same Rust implementation owns the documented operator lifecycle. Unlike a
Cucumber scenario, the `start` command launches one persistent owner process;
that process keeps MongoDB, all mock enclaves, all validator guards, and each
validator's OCOMP Supervisor, SnapshotExporter and Worker-0 alive until `stop`
signals that exact owner.

```sh
mise run build
mise run localnet-bootstrap
mise run localnet-start
mise run localnet-status
mise run localnet-stop
```

Bootstrap runs `outbe-chain dkg bootstrap`, `scripts/seed_genesis.py`, current
dynamic OCOMP founder registration for the complete genesis ValidatorSet, and
`tee genesis --mode gramine-direct-dev`. Start brings every mock enclave to
socket readiness before launching the validator cohort, then returns only after
all validator RPC heights have advanced beyond genesis. Runtime ownership is
not declared ready at that point alone: the harness installs the OCOMP delegate
bindings, observes the Supervisor embedded in each node, starts one external
SnapshotExporter and Worker-0 per validator, and requires every Supervisor to
report exactly one registered and connected Worker. `status` repeats the RPC and
node-owned Supervisor probes.
Runtime ownership is recorded atomically in
`<OUT_DIR>/localnet-state-v1.json`; stale or PID-reused
records are never treated as a live network. Bootstrap scans for free complete
service-port blocks and stores them in `<OUT_DIR>/localnet-bootstrap-v1.json`;
the persistent owner restores that exact layout, including genesis-baked P2P
and consensus endpoints.

The `outbe-e2e localnet-sgx` / `mise run localnet-sgx-*` surface is deliberately
fail-closed pending `outbe-chain-8lp`. A real-SGX network needs its own
`DcapRequired` genesis and hardware evidence; it must never reuse or fall back to
this mock `GramineDirectDev` lane.

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
| `PFS-002-01` through `-24` | Dynamic OCOMP membership, the validator system-vote lane, deadline accountability and independent FullNode Lysis following; the 4→5 process is the current reachable acceptance lane |
| `PFS-005-01`, `-09` plus named recovery/rejection tags | Vote approval/activation, restart boundaries, rejection paths, unsupported-version stall and operator binary replacement |
| `PFS-006-01`, `-02`, `-03`, `-04`, `-06`, `-09` | Join/exit/claim accounting, stale join, DKG recovery, slash idempotency, checkpoint restarts and full-committee sealed TEE recovery |
| `PFS-007-01` through `-12` | Pectra/ZeroFee readiness, one-atomic-unit EIP-7702 bootstrap, quota/fallback, exact replay, restart persistence, invalid authorization and day reset |
| `PFS-008-01` through `-08` | Cold/chained sync, upstream loss/switch, validator recovery, boundary restarts and idempotent warm promotion |
| `PFS-010-01` through `-04` | Shared policy, bonded Factory approval/refund, issuer ledger operations, duplicate-ticker rejection and same-binary full-committee restart |

Run one mapped example with `--tags '@pfs-001-05'`. A tag means that the
scenario supplies the evidence stated in its PFS matrix row; it does not imply
coverage of assertions that the row explicitly marks as a gap.

## Layout

- `features/` — eight responsibility-owned Gherkin suites: `tribute`,
  `validator_lifecycle`, `fullnode`, `ocomp`, `governance`, `zerofee`,
  `products`, and `tee_onboarding`. Shared prerequisites are steps inside the
  owning behavior, not standalone scenarios that repeat a shorter prefix.
  `ocomp.feature` owns the Linux-only fresh Metadosis path from finalized
  block-1 `Create` through terminal OCOMP, FullNode verification, NOD, and
  restart/replay on the same WWD.
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
- `--tee <real|sgx-no-attest|gramine-direct|mock>` — mandatory enclave mode
  (default `mock`). `sgx-no-attest` runs the production enclave under real
  `gramine-sgx`, uses EGETKEY sealing and the production NodeHost session, but
  deliberately disables DCAP, does not mount QVL, and submits only
  `GramineDirectDev` evidence.
  `gramine-direct` uses the production enclave binary without SGX. `mock` is
  test-only. None of the development-evidence modes proves remote attestation.
- `--no-sudo` — run scripts/docker without `sudo`.
- `--all` — treat an unsatisfiable scenario as a failure instead of skipping it.
- `--debug` — stream localnet setup output (bootstrap / process / docker) live;
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

## Dynamic OCOMP membership scenario

The OCOMP acceptance lane does not configure a result committee. It starts with
four ACTIVE validators only to create a visible membership transition, opens job
A (`N=4`, quorum `3`), synchronizes node 5 as a real FullNode and requires it to
materialize job A without voting, then restarts it in validator mode and drives
registration, stake, `PENDING`, delegation,
`confirmValidatorReady` and the certified DKG/reshare boundary. Job B must then
pin `N=5`, quorum `4`; node 5 may vote in B and must not vote in A, while A keeps
its original snapshot. Validator 2 completes both quorums; validator 3 remains
absent so both deadline summaries exercise the same missing participant before
and after its first `ACTIVE -> JAILED` transition.

Each finalized attempt gives its pinned participants exactly 1,800 blocks to
compute and submit a valid vote. Quorum may apply the result earlier, but it does
not close the remaining vote slots. At the exclusive deadline the deterministic
deadline transition records every pinned participant without a timely included
vote and jails only one whose current ValidatorSet status is still `ACTIVE`.
Votes use the canonical validator-authenticated OCOMP system carrier with
visible `gas_limit = 30_000`; its bounded internal work does not consume the
ordinary user-transaction gas lane.

The FullNode phase is not only a synchronization prelude. A FullNode has no OCOMP
signing key and never votes, but independently runs the same canonical Lysis,
retains its local result data, and refuses activation unless digest, roots and
manifest match the finalized quorum result. The process scenario must cover
matching, unavailable-input/mismatch fail-closed behavior and restart recovery.

Those counts are scenario inputs, not OCOMP constants. Fixture generation reads
the ordered validator manifest, produces one founder registration per genesis
validator and contains no static committee or threshold artifact.

Actually executing a scenario needs a Linux box with `sudo` + `docker` + `gramine`
(same prerequisites as `mise run e2e`). First build the binaries the steps call:

```sh
cargo build --release -p outbe-chain --features e2e-test,test-protocol-overrides --bin outbe-chain
cargo build --release --bin outbe-cli
cargo build --release -p outbe-tee-enclave --bin outbe-tee-enclave
cargo build --release -p outbe-tee-enclave --features mock --bin outbe-tee-enclave-mock
```

Then, e.g.:

```sh
# Omit --projection-mongodb-uri to use the harness-owned replica set.
# Through the mock enclave on an isolated GramineDirectDev chain.
cargo run --release -p outbe-e2e-harness --features ocomp-integration --bin outbe-e2e -- \
  --tee mock --validators 4
# a fully-capable box: everything must run (unmet ⇒ fail, not skip)
cargo run --release -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock --validators 4 --all
```

Compile the dynamic OCOMP acceptance source without starting processes:

```sh
cargo test --locked -p outbe-e2e-harness \
  --features ocomp-integration --no-run
```

After integration with the current node/Supervisor/worker transport, run the
focused process lanes:

```sh
cargo run --locked --release -p outbe-e2e-harness --features ocomp-integration \
  --bin outbe-e2e -- --tee mock --validators 4 --all \
  --tags '@ocomp-dynamic-admission'
cargo run --locked --release -p outbe-e2e-harness --features ocomp-integration \
  --bin outbe-e2e -- --tee mock --validators 4 --all \
  --tags '@ocomp-dynamic-overlap'
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
Its port is published only on host loopback, so the managed replica set works
with rootless Docker and Docker Desktop without host networking.
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
cargo run --release -p outbe-e2e-harness --bin outbe-e2e -- \
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

## Validator lifecycle consistency target suite

[`validator_lifecycle_consistency.feature`](./features/validator_lifecycle_consistency.feature)
contains target-state checks derived from
`.cursor/validator-lifecycle-consistency-e2e-checklist.md`. It covers 16
public/runtime-reachable checklist IDs without direct storage writes. All current
scenarios are enforced; none is tagged `@expected-to-fail`. If the tag is used
for a future target invariant, it remains a selection and traceability tag, not
Cucumber skip or xfail semantics.

Run the currently enforced subset:

```sh
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee mock --validators 4 --all \
  --tags 'not @expected-to-fail' \
  --input testing/e2e-harness/features/validator_lifecycle_consistency.feature
```

There are currently no target-gap examples. Setup errors, environment failures,
timeouts, lack of finalization, and log-audit failures remain distinct from
product-defect evidence.

### Coverage matrix

| Checklist ID | Live scenario | Target result |
|---|---|---|
| D-01A | Replay one unchanged registration fixture after a valid cross-chain rebootstrap | Replay rejected |
| D-01B | Configured owner submits an empty-PoP post-bootstrap registration | Registration rejected |
| D-02 | Readiness arrives just after freeze | No immediate DKG; activation at the next scheduled window |
| D-04 | Attempt to omit an ACTIVE member and recover it at the next reshare | `ACTIVE/share=false` remains repairable |
| D-05 | Submit canonical felony evidence and all replays | Exactly one complete punishment |
| D-06 | Claim and same-EOA re-register in one block | Dense index retained; reachable residue and key ownership reset |
| D-07 | Partially unstake a jailed validator below minimum | Validator remains JAILED |
| D-10 | Request unjail before exclusion | Rejected or `PENDING/share=false` |
| S-01 | Drop confirmed PENDING below minimum, then restake without reconfirming | Readiness cleared and no activation |
| S-02 | Owner calls the public raw activation facade | Rejected with the coupled bundle unchanged |
| S-03 | Duplicate, reorder, or hash-mismatch an activation set | Three atomic rejections |
| S-06 | Finalize successful and reverting lifecycle value transitions | Stake mirrors and native value remain conserved |
| S-08 | Consume staggered claims one at a time | INACTIVE only after all bonded/live value is gone |
| S-09 | Submit three invalid P2P updates, then reset identity | Pair is atomic and both fields clear together |
| S-12 | Submit old evidence after an unchanged committee wraps the snapshot ring | Evidence rejected without punishment |
| S-14 | Submit invalid-PoP, duplicate-key, and over-capacity registrations | No partial identity, reverse-owner, index, or count writes |

### Fixture policy and deliberate exclusions

Scenario genesis customization is typed configuration only. It may use distinct
valid chain IDs, select an existing validator EOA as ValidatorSet owner, set a
zero re-registration cooldown, set capacity at or above the initial validator
count, and shorten positive epoch/DKG/unbonding windows while preserving their
normal ordering constraints. It must not write storage slots, seed an unknown
status, pre-load near-overflow counters, or otherwise create a state that cannot
be reached through production interfaces.

D-08 and S-13 are excluded because they require respectively corrupted status
storage and counters seeded near their maximum. The INFO-only D-03, D-09A
through D-09F, S-04, S-05, S-07, S-10, and S-11 remain system, CLI, or
fault-injection work rather than live-node scenarios. The implemented live
boundaries are also intentionally limited as follows:

- S-01 covers the public unstake path; its internal slash-transition variant
  needs a controlled system hook.
- D-06 verifies naturally reachable re-registration residue, not synthetic
  stale jail/readiness values.
- S-06 verifies committed and publicly reverting value transitions, not
  injected mid-transaction failures.
- S-14 covers validation, uniqueness, and valid capacity rejection; a late-write
  failure still belongs in an atomicity fault-injection test.

There are no `@todo` placeholders for excluded checks.

## Published production SGX release acceptance

`outbe-release-sgx-e2e` is a second Rust/Cucumber entrypoint for one exact published
`image@sha256:...`. It is separate from the localnet World because release acceptance owns
an immutable bundle/image/sealed-state fixture, not validators or MongoDB:

```bash
cargo run --release -p outbe-e2e-harness --bin outbe-release-sgx-e2e -- \
  --network testnet \
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

Only after every step passes does it write canonical `hardware-sgx.json`. This
proves immutable SGX execution and sealing, but it is still not remote-attestation
evidence. Accepted DCAP evidence is produced by a separate exact-release runner:

```bash
cargo run --locked -p outbe-e2e-harness --bin outbe-release-dcap-evidence -- \
  --network testnet \
  --image 'ghcr.io/outbe/outbe-tee-enclave-testnet@sha256:<64-hex-digest>' \
  --bundle /tmp/extracted-signed-sgx-bundle \
  --genesis release/testnet-genesis.json \
  --expected-pck-ca processor \
  --output-dir /tmp/hardware-dcap-processor
```

The command requires the project-pinned host-only QPL, working PCS/QCNL
configuration, real SGX devices and an image that was already Cosign-verified and
pulled by digest, plus the tracked final testnet genesis whose block-1 policy
authorizes the signed measurements. It records host topology as untrusted
provenance before creating a binding or contacting PCS, generates a fresh
intent-bound quote inside the release Gramine enclave, acquires all eight
collateral components, and requires an `Accepted` result from the public
enclave-resident Begin/Chunk/Finish verifier. It retains canonical evidence,
verifier bytes, actual CRLs and non-secret provenance only.
`--expected-pck-ca platform` is an on-demand compatibility and node-admission
diagnostic, not a release row: the Intel-signed PCK issuer is the authority,
while guest-visible socket count is never used as a CA verdict.

The protected workflow uses the single fixed path
`release/testnet-genesis.json` from the immutable testnet tag. A missing or
untracked file is a release failure; the harness never synthesizes a replacement.

## Focused Tribute compressed-entity checks

Run the complete Tribute compressed-entity feature (happy path and edge cases):

```sh
cargo run --release -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee gramine-direct \
  --validators 4 \
  --input 'testing/e2e-harness/features/tribute.feature'
```

Run only the creation happy path:

```sh
cargo run --release -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee gramine-direct \
  --validators 4 \
  --name "One public Tribute has complete projection and duplicate protection"
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
