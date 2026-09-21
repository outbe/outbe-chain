# Ordinary recovery: current-main evidence

Baseline `e5d545ce`; Tier2 graph generation2026-09-04 was stale/excluded for bin
and newer split files. Agents checked coverage and read current source for the
material paths. This is source evidence, not a successful copied-store run.

## Reusable behavior

- `engine/src/stack/recovery/anchor.rs`: validator selects minimum durable
  execution/finalized progress; follower bounds certificate/block/execution tip
  against Marshal processed progress. Preserve native archives/metadata.
- `consensus/src/executor/actor/finalization.rs`: exact recovered height/hash is
  ACKed, subscribers/observer notified, without EVM reexecution. Existing tests
  cover recovered ACK, conflicting hash and CE commit barrier.
- `bin/outbe-chain/src/ocomp_exex/run.rs`: load native closure C, verify canonical
  hash and projection coverage, initialize scanned progress from C, then read
  canonical frames from C+1. Empty runtime maps do not imply start from zero.
- `ocomp_exex/discovery.rs`: reuse retention/discovery authority and completed
  local results. No snapshot-specific recreation of a completed job is needed.
- `ocomp_exex/materialization.rs` and `bin/outbe-ocomp/src/embedded_runtime.rs`:
  current canonical FIFO can select pending NOD work after terminal runtime jobs
  are forgotten; copied CAS/input/admission records feed the ordinary path.

## Existing payout behavior is outside snapshot changes

The current payout callback selects the current date and30prior dates and reads
finalized contract state plus local public artifacts. It does not replay old
blocks. This behavior is unchanged by snapshot creation, direct file placement
or optional validation. Extending its search window or adding historical
scheduling is outside this feature; the former task07 was cancelled, not
implemented. Preserve the public files and test ordinary continuation within
the existing rules. Read-only offline artifact inspection does not promise that
the running node will select every historical unpaid round.

## Native prerequisites, not new bootstrap requirements

Follower reads genesis ValidatorSet state and uses configured upstream certified
epoch history (`consensus/src/follow/engine.rs`). Validator DKG recovery can need
freeze header/state (`engine/src/stack/dkg/{startup,handoff}.rs`). Preserve available
history and test current fixture prerequisites; a current state root alone does
not establish that arbitrary pruned H-only stores can start.

Missing closure initializes genesis and missing Marshal metadata loses processed
progress. Creation and direct file placement must retain them; never manufacture H cursors. Local
fatal evidence remains meaningful and must not be silently cleared by file placement.

Task08's native archive fixture must keep the same partition prefix across stop,
copy and reopen: current helper generates a fresh atomic test_id otherwise.
Task09 owns real process proof after normal shutdown of all writers. A quiet
completed-Lysis cut is preferred but not mandatory; creation fixtures prove no completion gate. Cross-identity migration of donor
unfinished computation is not required by process acceptance.

## Direct native-copy audit, 2026-09-20

The coordinator and two independent reviewers checked the ordinary chain/CE and
OCOMP startup paths against current main. The scoped scenario is a stopped copy
after Lysis, containing completed public outputs and any remaining ordinary NOD
or payout inputs. Transferring unfinished donor computation is outside scope.

**Conclusion:** the traced readers support direct use of ready native files at
the recipient's configured paths. No finding establishes a need for a snapshot
restore command, deployed-state reconstruction or a snapshot startup lifecycle.
This does not yet prove a whole copied node starts: that process test has not
been executed. The payout search-window observation above does not require a snapshot change
or block snapshot acceptance.

| Domain | Existing native behavior | Required acceptance evidence |
|---|---|---|
| Chain/Marshal | `stack/follower.rs` opens the ordinary `outbe-marshal` partitions and authenticates its reconciled anchor against execution and processed progress. `stack/recovery/anchor.rs` enforces its bounds. The exact already-canonical block is acknowledged without EVM reexecution. | Copy actual archives and processed metadata with a stable partition prefix. Observe the copied anchor, successor processing and later current-height restart. |
| CE | `launch/node.rs:688-748` opens the existing native store. `ce_recovery.rs:139-249` resumes an equal full marker without replay, replays only a lagging suffix, and rejects an ahead/conflicting marker without rewind. | Use the real Reth provider: `ce_finalizer.rs:150-252` reads header/root evidence at reconciled A and A-1 even for an equal CE marker. A lagging marker additionally needs suffix receipts/state. An equal scalar height or current-root-only audit does not prove those reads succeed. |
| Projection | `projection/startup.rs:114-209` retains its canonical checkpoint. `projection/sink.rs` skips already applied frames. A checkpoint above transiently stale provider finality waits; it is not truncated. | Actual copied backend reaches readiness after provider recovery. Preserve native conflict/missing-history errors; do not manufacture matching markers. |
| OCOMP | `ocomp_exex/run.rs:180-258,434-443` verifies copied closure C and projection coverage, then reads frames from C+1. `embedded_runtime.rs` opens public CAS/admissions/results at recipient paths. Inspected public records are chain/job/content-bound rather than donor signer/absolute-path-bound. | Include all referenced public outputs and installed bundles, native file modes and ordinary recipient provisioning. Copied donor signed journals are excluded, not adopted as recipient authority. |
| Pending public actions | `materialization.rs:128-178` reads the canonical NOD FIFO independently of retired discovery jobs. Copied input/admission/CAS data supplies remaining batches. `run.rs:544-580` triggers NOD/payout on a newly processed finalized frame, not immediately on a quiet C=H startup. | Demonstrate pending NOD and recent payout after the next eligible finalized frame with the recipient's own authorized signer. FullNode does not submit validator transactions. Preserve the existing payout candidate window. |

OCOMP's ordinary root is a sibling of the resolved chain datadir
(`launch/node.rs:415-428`); copying only the Reth directory is insufficient.
The nested NOD references, bundle catalog and native file metadata described in
the data-layout document remain part of the artifact contract. Genesis state,
required DKG freeze history and upstream epoch history are existing startup
inputs, not something reconstructed from a snapshot manifest. Current follower
startup reconstructs its committee chain from upstream epoch history; this must
not be confused with replaying all EVM blocks from genesis.

No clean stopped post-Lysis copy was demonstrated to fail in this audit. Missing
A/A-1 provider evidence, conflicting native markers and omitted public outputs
are conditional failures, not evidence of a newly discovered shutdown bug.

## Executed checks and remaining proof

Current-main commands, with `SOURCE_DATE_EPOCH=0 RAYON_NUM_THREADS=4`:

```text
cargo test --locked -j4 -p outbe-engine --lib recovery_anchor -- --nocapture
6 passed; 0 failed

cargo test --locked -j4 -p outbe-engine --lib ce_recovery::tests -- --test-threads=4
6 passed; 0 failed
```

The first group checks anchor bounds and exact verified records. The CE group
uses MemorySource/MemoryTree and checks equal/behind/ahead/conflict/gap behavior.
These are executed component checks, not real Reth/MDBX copied-node acceptance.
Existing native CE tests prove same-directory reopen only; they were inspected,
not executed in this audit. No whole-process copy/start run, full workspace
quality gate or E2E result is claimed. Tasks08/09 own that remaining proof.

No source finding requires changes to consensus, DKG, launch, payload handling,
shutdown, or a new startup reader. Future implementation failures require exact
root-cause evidence; this report is not authorization for a broad runtime rewrite.
