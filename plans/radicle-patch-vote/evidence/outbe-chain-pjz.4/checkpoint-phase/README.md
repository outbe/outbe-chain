# Atomic publication checkpoint and system phase

Source: `259df4bec745acdcbd656a08e49d782b1e889e94`. Intermediate P3 implementation evidence; P3 remains incomplete and has no independent full-AC acceptance PASS.

`checks.json` records source/cwd/toolchain/platform/environment, actual argv, UTC start/end times, exit codes, spec hashes and raw-log hashes. All six commands exited0, with no source changes during verification:

- Seven complete lib suites: consensus506, engine267, EVM271, node201, primitives388, outbe-radicle38, registry28: **1699 passed,0 failed,0 ignored**.
- Three EVM integration suites: **23 passed**, including system layout, legacy E2E envelopes and phase1 safety/variant compatibility.
- Node consensus-stateless and visible fee-history integration suites: **14 passed**.
- Eight selected production consumers: all-target check PASS.
- Seven selected packages: scoped Clippy `--no-deps --lib --tests -D warnings` PASS. This is not whole-workspace lint or remote CI.
- Scoped Rust formatting PASS.

The Python genesis code and fixture bytes are unchanged from5a7bd5153; its prior98-test Python regression evidence (2 keygen-binary skips) stays in `../publication-carrier`.

| Implemented requirement | Source | Actual behavioral evidence |
|---|---|---|
| Current epoch/version/full-polynomial availability, group key, exact effective addresses/MinPk keys, strict SDK player order and OPL1 commitment | `crates/system/radicleregistry/src/publication.rs::publication_committee`; shared `primitives/src/radicle_publication.rs::players_commitment` also used by `radicle/src/integration/signing.rs` | Real participant-bound Commonware DKG/current committee fixtures; foreign chain/genesis/descriptor/epoch/version/group/players context rejects without state effects; existing38 native signing/authority tests remain PASS |
| Each current indexed MinPk signature covers exact OPB1 bytes under fixed `outbe.radicle.publication.v1`; max32 verifications per batch | `publication.rs::apply_publication_batch`; `primitives/src/radicle_publication.rs` | Wrong key/domain/index, zero/duplicate frames and typed33-entry batch reject. Actual n256/q171 test signs with256 real identity keys and accepts6 batches, none over32 |
| All entries, freshness, body CAS and conflicting slots are validated before effects | `publication.rs::apply_publication_batch` | Bad last entry after a potential quorum crossing leaves storage and events unchanged. n25632-entry invalid-last batch also leaves storage unchanged |
| Fixed pending slots by participant index; duplicate cannot inflate count; conflicting body cannot reuse a slot within actual current generation | schema slots19/20; generation tag binds current epoch/version/full polynomial/players/sequence, not candidate material hash | Repeated identical index tallies once; alternate material/body for the same participant rejects; one conflicting participant does not prevent the other3-of4 from reaching the agreed body |
| One bounded tally and one immutable receipt when q is reached, including q crossed inside a batch | `publication.rs::{apply_publication_batch,commit_publication,publication_receipt}` | 4-entry batch crossing3-of4 commits sequence1 once; replay fails CAS; receipt2 remains absent. Raw dynamic receipt length is bounded before loading |
| Generation transition resets unfinished slots and cannot revoke accepted history | current-state generation tag plus immutable receipt map | State-generation fixtures reject old epoch/version attempts; new generation does not inherit old pending count; BindMaterial preserves receipt1. Actual real SDK reshare/old-share rejection remains covered by the consensus suite from the earlier foundation |
| Exact terminal Approved patch subject; Review and Merge stages separate; approval row immutable | `validate_publication_body`, `patch_approval`, separate stage map | Real public Vote create/cast/begin-block finalization, wrong revision rejection, Merge-before-Review rejection, separate Review/Merge receipts, unchanged original approval payload and previous receipts |
| Automatic managed RID binding adopts an existing registry row without granting its registrant ownership; capacity reserved until bootstrap | `runtime.rs::register_repository`; `publication.rs::commit_publication`; append-only schema12..20 | Enabled capacity2 reserves one slot; both fresh RID and pre-registered RID bootstrap; existing index and recorded registrant remain unchanged. No public ABI setter installs a managed binding |
| Whole checkpoint is atomic even when writes/events fail | enclosing existing checkpoint guard | **34 mutation sites**, failure injected before and after each; pending/binding/registration/receipt/sequence and events all roll back |
| Optional OSP2 V2 system phase after HookEvents; publication calldata must equal header tag09 batch | `primitives/src/system_tx/{input,kind,layout,phase,binding}.rs`; actual EVM builder/verifier/dispatch paths | Canonical3683-byte max calldata, strict decoder, exact header-body parity, missing/extra/mismatched phase rejection, legacy absence preserved; all system-tx codec regressions PASS |
| Production begin-block dispatch requires SYSTEM caller, rejects value and invalid signatures atomically | `evm/src/begin_block_precompile/mod.rs` | Actual dispatch test with real DKG/current identities; external/value/bad-last rejects before state changes; correct quorum records receipt |
| Proposer signed body and verifier inputs agree for the new phase | `evm/src/config/system_txs.rs`, `executor/system_execution/{layout,dispatch}.rs` | Actual builders at heights1/2 with/without OCOMP and with boundary; byte-identical inputs including publication after HookEvents; visible-gas planning includes the phase. Invalid phase results reject a payload rather than stopping the node |

Paths abbreviated `primitives`, `radicle`, `evm` are under `crates/blockchain/`. The exact changed-file/caller/fixture list is in `checks.json`; schema layout tests pin old slots0..11 and new append-only slots12..20.

## Measured bounded work

The frozen n256 fixture forms an actual participant-bound DKG, then measures checkpoint application separately from fixture setup. q171 completes in6 valid batches. Maximum observed per valid batch: **12245 storage reads,64 writes,1544500 storage gas** under the existing system-call budget. The log prints total time for valid plus negative calls, not a production end-to-end latency guarantee. Signature verification is capped by the checked1..32 batch count; tally scans at most the existing256-member bound and does not reverify q cached signatures.

These are current-identity checkpoint endorsements. They do not replace the real threshold BLS native owner signatures, whose fixed-domain/current-polynomial provider and native Review preparation remain in the earlier foundation and current38-test suite. The registry does not parse Git or Vote inside Heartwood.

## Still incomplete

The dispatch test invokes the production precompile entrypoint with an explicit state provider; it is not a live-node/full-revm-block native publication scenario. The builder/verifier test proves input/envelope agreement, not execution of a live distributed job.

The production post-finality collector, active authenticated DKG Output provisioning, native bootstrap/refs/Merge actor subjects, authenticated node-sidecar exchange, durable recovery, fair Approved-job scheduling and refill/ack of the retry-preserving mailbox are outstanding. No production actor supplies the mailbox yet.

The validating node's authenticated-finality/ancestry admission for the publication anchor is not wired. The storage runtime checks the deterministic anchor height and a canonical hash exposed by its provider; it does not prove finality. Behavior when the needed canonical history is unavailable remains to be completed/proven with that node admission. These tests do not establish archive/snapshot-join behavior.

Automatic native governments materialization/RID, initial content artifact validation, manager receipt-policy provisioning, actual native threshold Review-to-announcement-to-fetch and full multi-validator E2E remain incomplete. Library tests use explicit current-state/DKG/manifest fixtures; they do not claim those production scenarios. P2/P3/P4/P6/P7/P8 remain open or in progress; no task closure, reviewer PASS or goal completion.

No edits to the reverted `vendor/heartwood/crates/radicle/src/storage/git.rs` gate or D2 work. Failed development compile/lint attempts are separately preserved in `development-failures/` and its hash manifest; they are not frozen-source checks.
