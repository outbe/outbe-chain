# P1 — stable Heartwood update

Implementation candidate: `eb834f31ccd027d7af604ba1393498491cc133e6`.
Base: `0a322cf3c` (P0 acceptance evidence; production base remains `42d08693a`).
Task: `outbe-chain-pjz.2`. Live description hash is recorded in `environment.json`.
P1 remains open until three independent full-AC reviews pass on this candidate.

## Source transfer and retained contracts

`upstream-transfer.json` records the actual three-way transfer from upstream
`16de8f1950825672fe48b901dafa0d009f612b04` to stable tag `releases/1.10.3`, peeled
`7e1cb406bb935c3de008d2171e8961f57b4336ea`. The prior Outbe fork is
`b76a17801329291153585ed31db61ee3c658046e`. All 67 upstream changed paths were
transferred. Two intersecting paths were merged: CLI Cargo metadata and node config.
The transfer hashes describe the import operation; subsequent formatting/provenance
adjustments are represented by the final manifests, not silently equated with upstream.

`vendor-tree.json` accounts for all **817** tracked vendor files, with modes, Git blob
IDs and SHA256. The complete upstream file set is present; `UPSTREAM.md` is the sole
extra vendor file. `retained-delta.json` accounts for every difference against stable:
the original Outbe control/config/build/manifest delta, updated provenance and one
upstream Markdown trailing-space removal. `retained-contract-check.json` verifies
the untouched delta byte-for-byte against base and names both adapted source files.

Outbe's native control UDS remains available inside its protected 0700 parent. The
loopback HTTP server remains read-only. At this base there is no public raw-control
RPC proxy or raw-control enable flag: `crates/blockchain/rpc/src/api.rs:457–467` exposes only status,
peers and repositories. The plan's "raw control disabled by default" requirement is
preservation of that boundary, not evidence of a nonexistent optional flag. RPC and
launch wiring are unchanged; the migration introduces no remote control endpoint.

## Actual changed-files, callers and fixtures

`changed-files.json` contains the full changed-file/hash inventory. This table maps
the behavioral reach; a listed unchanged caller was inspected, not needlessly edited.

| Change / retained boundary | Actual callers, fixtures and validation |
| --- | --- |
| Vendored stable identity/COB/canonical refs/fetch/storage API | All upstream changes in transfer manifest; `radicle --lib` 398 tests and `radicle-node/radicle-protocol --lib` 190 tests, including native peer fetch, thread/identity and signed-ref cases |
| Outbe `Network::Outbe`, no public bootstrap | `vendor/heartwood/crates/radicle/src/node/config.rs:80`, `profile/config.rs:155`; adapted `outbe_network_has_no_public_bootstrap`, sidecar `options_build_the_exact_outbe_profile_and_limits` and runtime config/peer checks |
| Bounded disconnect and native control traits | `radicle/src/node.rs:1030,1342`, `node/command.rs:73`, node control/runtime handle and protocol service/command; chain `crates/blockchain/radicle/src/manager/adapters.rs` retains exact UDS call/disposition contract; sidecar real UDS test covers all frozen dispositions |
| Status max connections and deadline | `crates/system/radicle/src/lib.rs:235,273`; accepted worker socket is explicitly blocking, read deadline is absolute; existing 33rd-connection rejection/stall test plus new trickled-input regression, both real TCP |
| Extracted runtime and sole entry point | `crates/system/radicle/Cargo.toml`, `bin/outbe-radicle/{Cargo.toml,src/main.rs}`; root all-target check and actual process peer scenario; no duplicate vendor runtime introduced |
| Local dependency graph / keys | Root Cargo paths/lock and vendor workspace/lock; `bin/outbe-keygen/src/main.rs:338` uses unchanged Ed25519 Keystore API; root all-target check includes keygen and E2E ocomp-integration callers |
| CLI, helper and build metadata | Retained `vendor/heartwood/build.rs`; `testing/e2e-harness/src/artifacts.rs:329` installs vendored paths and fingerprints binaries; `world/localnet/radicle.rs:743,1720` verifies chain revision; actual peer script checks both binary `--version` revisions, clone, identity, sigrefs and helper `ls-remote` |
| Wire / legacy storage compatibility | `wire-source-compatibility.json` checks unchanged crypto/framing/message sources; real legacy b76a178 CLI+sidecar author → stable sidecar/CLI/helper clone, identity and signed refs validation; native suites exercise full storage paths |

## Acceptance and evidence

The authoritative final executions are `final/root-commands.json` and
`final/heartwood-commands.json` plus `final/root-resumed-commands.json`: exact
argv/cwd/SHA/time/exit/environment/log hash.
`environment.json` records platform/toolchain/spec hash. Each final command must
have exit 0, and each expected test must actually execute. Peer commands and
binary hashes are in their final command records, logs and transcript directories.
Earlier exploratory logs are retained separately and are not substituted for final runs.

| AC | Source / actual proof |
| --- | --- |
| AC1 stable source and complete Outbe contracts | Ancestry/transfer/full vendor/retained manifests; complete source map above; full native suites; real UDS dispositions, status limits and peer scenarios |
| AC2 both locks local, no old fork download | Root lock adds only local `radicle-windows`; vendor lock is stable release; final full `cargo metadata --offline --locked` graphs enumerate local Heartwood manifests and no old Git source; both required builds run offline/locked |
| AC3 sidecar/CLI/helper and current metadata | Final real same-version peer and legacy→stable peer runs; `rad init`, real network clone, exact commit/content, identity delegates, verified sigrefs feature level, helper `ls-remote`, bounded disconnect; binary version checks against exact candidate Git HEAD |
| AC4 actual limit/replication tests and required checks | Final root/standalone metadata and fmt, required root all-target check, standalone CLI/helper check, both Clippy suites with warnings denied, all10 sidecar tests, native190+398 tests, harness11 artifact tests and exact version regression, actual peer transcripts |

Known baseline: `baseline-status.log/.json` reproduces the 33rd-connection failure
before this patch on Darwin. Accepted sockets inherited O_NONBLOCK, so workers
exited on partial requests instead of occupying capacity. The shared worker now
sets blocking mode and checks timeout setup failures. Its absolute read deadline
also prevents a byte trickle from extending the five-second input budget. The
existing test now waits for released capacity before its independent stall probe,
so worker cleanup scheduling is not mistaken for deadline failure.

The first full root check failed because sandbox denied Nargo's dependency-cache
lock (`root-check-sandbox-failed.*`). The same required check passed with cache
access, without source/toolchain/dependency workarounds. This failed attempt remains
visible and is not called PASS. Pinned Nargo/BB compile the real circuits normally.

The first final harness-artifact attempt was interrupted: the unit fixture
`Environment::default()` blocked in the local Docker Desktop `docker info` probe
(stack sample and interruption record retained). These tests do not execute
containers. They are rerun with explicit `DOCKER_HOST=unix:///tmp/outbe-p1-no-daemon.sock`
and unset `DOCKER_CONTEXT`, so the unavailable-daemon path returns promptly. The
interrupted attempt is not PASS; the resumed record must show all11 assertions
complete. Real native sidecar/peer tests use real sockets and no Docker substitution.

No chain toolchain upgrade or unrelated dependency update was made. Rust1.96.0
from the chain root satisfies the upstream MSRV1.88.0. The upstream standalone
developer toolchain file1.98.0 is retained; it was not installed or used for these
chain-root `--manifest-path` executions.

Scope: P1 only. No BLS ownership/Vote/collector/genesis implementation is claimed.
Validation is local Darwin arm64; remote CI and Linux/SGX full-chain E2E were not
run for this update. Those are not substituted for P8's required production E2E.
