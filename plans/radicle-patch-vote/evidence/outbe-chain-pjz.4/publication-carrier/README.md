# Bounded carrier, proposer mailbox and genesis descriptor prerequisite

Source: `5a7bd515322d61914fe57e4319ab31da3c1c7d2c`.

This is intermediate implementation evidence for P3, including the descriptor prerequisite for its opt-in activation. P3 and P4 are incomplete. This bundle does not claim full acceptance criteria, independent reviewer PASS, or a production Vote-to-Review flow. `checks.json` records each completed frozen-source command and its actual exit code; missing or running checks are not PASS.

| Implemented requirement | Source | Behavioral proof |
|---|---|---|
| Fixed OPB1 body477, exact action encoding, 1..32 strictly ordered attestations, payload max3678 | `crates/blockchain/primitives/src/radicle_publication.rs` | Exact independent SHA256 layout vector; all valid batch counts; wrong magic/action/count, duplicate/unsorted indices, truncation/trailing bytes reject |
| Independent publication tag09 alongside DKG, enabled OART0C, disabled legacy0B unchanged | `primitives/src/reshare_artifact.rs` and `reshare_artifact/records.rs` | Coexistence with each consensus artifact; duplicate/legacy-wrapped publication rejects; empty enabled envelope explicit; existing artifact golden/regression tests |
| Carrier-sized ready mailbox, nonblocking proposer read, no loss on abandoned candidate | `primitives/src/radicle_publication.rs::RepositoryPublicationMailbox`, `primitives/src/consensus.rs::ConsensusExecutionBridge` | Ready prefix survives repeated reads; held collector lock yields no publication; invalid replacement preserves old batch; explicit clear |
| Actual proposer takes maximum fitting prefix, mandatory DKG/late credits preserved, final summary/timestamp/root reserved within unchanged64KiB | `primitives/src/reshare_artifact.rs::encode_proposal_block_artifacts`; `consensus/src/application/handler/proposal/attributes.rs::encode_proposal_header`; `engine/src/stack/epoch/run.rs` | Prefix counts0/1/7/31/32 with actual codec-sized dealer log and final execution fields at exact64KiB; max mandatory late credits plus full32 fits; sanitizer preserves publication |
| Activation derives from descriptor hash committed in genesis storage | `node/src/consensus/policy.rs::validate_repository_publication_mode`; `engine/src/stack/startup.rs::repository_publication_enabled_from_genesis` | Matching modes pass; enabled/disabled mismatch and missing/unseeded/changed config descriptor reject; disabled opaque legacy header behavior preserved |
| Effective pre-freeze publication window at least64 for enabled startup and seeding | `primitives/src/radicle_genesis.rs::validate_publication_window`; `engine/src/stack/epoch/run.rs`; `scripts/genesis_seed_alloc.py` | Default300/30 and shortest65/1 pass; insufficient/clamped windows reject; disabled seeding preserves old behavior |
| Cycle-free OGD1 exact P0 bytes/hash, strict UTF8 byte bounds and fixed governments/main/BLS policy | `primitives/src/radicle_genesis.rs` | Frozen P0 preimage/hash vector; invalid version/name/branch/policy/source/description/commitments/truncation/trailing bytes reject |
| Seeder commits exact descriptor bytes and hash; registry slots0..9 remain unchanged,10/11 appended | `scripts/genesis_seed_alloc.py::seed_governments_descriptor`; `scripts/genesis_seed_system.py`; `system/radicleregistry/src/schema.rs` | Real apply_seed opt-in/legacy pipeline, Python/Rust shared allocation fixture, typed registry decode and storage layout tests |
| Typed descriptor read bounds raw Solidity byte length before conversion/allocation | `system/radicleregistry/src/runtime.rs::governments_descriptor` | Missing/hash-mismatched/zero/oversized/noncanonical storage rejects, including U256::MAX length; default absent descriptor remains None |

Paths under `primitives`, `consensus`, `node`, `engine` above are relative to `crates/blockchain/`; paths under `system` are relative to `crates/`. The exact full changed-file list is in `checks.json`.

The source does not alter `vendor/heartwood/crates/radicle/src/storage/git.rs` or reintroduce the reverted fetch gate. No D2 work.

## Explicit remaining implementation and proof

The new carrier is not yet dispatched as a dedicated system transaction or applied to registry checkpoint state. Current-set endorsement verification, atomic all-entry validation/CAS/tally, immutable receipts, material binding, node finalized-anchor admission and receipt-backed publication policy remain incomplete. No production collector writes the mailbox yet; the mailbox is its bounded transport seam, not a threshold collector or a job queue. Fair Approved-job scheduling belongs to the still-unimplemented post-finality actor.

The seeder validates descriptor commitments and encoding, not the actual initial tree/content manifest artifact yet. Automatic native governments bootstrap after authenticated initial DKG, reserved registry capacity, descriptor-to-RID binding, manager policy provisioning and restart/join are outstanding P4 work. Native refs/bootstrap/Merge signing actor paths, authenticated sidecar exchange, durable recovery and multi-validator E2E are outstanding. Header/config tests and library codec tests do not replace these scenarios.

## Development diagnostics

`development-failures/` and its JSON manifest preserve failed environment/compile/test attempts from the evolving worktree. They are not frozen-source proof. The first malformed descriptor test exposed U256-to-usize conversion before a bound; the committed implementation checks raw storage bounds first. The unrestricted development Clippy run stopped in unchanged `outbe-tribute`; frozen scoped Clippy uses `--no-deps` and does not assert a workspace-wide lint PASS. Python checks of the two existing seed modules retain their pre-existing F401/TRY004 diagnostics; the new Python test module has a separate lint command. The Python regression suite has two explicit keygen-binary skips, not completed key-generation scenarios.

## Frozen check results

All seven commands in `checks.json` exited0, with no source changes during verification. Full lib suites: consensus506, engine267, EVM269, node201, primitives385, registry19: **1647 passed,0 failed,0 ignored**. Eight selected production consumer packages passed all-target check. Scoped five-package Clippy `--no-deps --lib --tests -D warnings`, scoped Rust formatting, three Python files formatting and new Python module lint PASS. Python genesis regressions ran98 tests with2 keygen-binary skips (96 completed). This is not an E2E or whole-workspace CI claim.
