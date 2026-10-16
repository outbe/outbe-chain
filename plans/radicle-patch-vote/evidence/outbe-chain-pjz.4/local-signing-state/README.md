# P3 local signing-state evidence

Intermediate source `d64b9ca9275a77ac02ed863517855342ddacf817`; base `f88a7f9c7a1f0822cf5ad247de16bbd759d28662`.

The protected local node socket now supports one fixed request `OCS1 || proposal_id:u256be` (36 bytes) in addition to unchanged ORL1 receipt reads. An OCS1 response is status0 (unavailable) or status2 plus one finalized block, chain/genesis, current epoch/material version, public polynomial hash/group key, bounded ordered committee, exact immutable approval, checkpoint/material receipts, stage and pinned anchor. The maximum payload derives from existing n<=256 and canonical Vote payload<=512 bounds; no secret material is encoded.

| Required contribution | Source | Behavioral proof in full-libs.log |
| --- | --- | --- |
| P3 AC1: authenticated finalized context for exact proposal | integration/local.rs:173,236,257; manager/snapshot.rs:93 (existing exact-state reader) | local_signing_state_is_requested_fresh_for_the_exact_proposal: actual Unix socket, full typed-state equality, next snapshot/stage visible, wrong proposal/network rejected, unavailable reader has no cached fallback, no separate authority sample |
| P3 AC1/AC2: bind decoded fields and reject mixed context | integration/local/state.rs:83 | signing_state_wire_rejects_mixed_contexts_bad_bindings_and_noncanonical_frames: epoch/version, ordering/duplicates, approval height/payload, receipt integrity, stage/anchor; every truncated prefix and trailing bytes rejected |
| Existing protocol bound | integration/local/state.rs:23 | signing_state_wire_supports_the_protocol_maximum_committee: real256-player centralized test DKG output and sorted identity keys round-trip;257 rejected. This is wire evidence, not a production/interactive DKG claim |
| Existing private socket/receipt behavior | integration/local.rs and local/tests.rs | all prior private modes/UID checks, active/stale owner, inode cleanup, malformed/no arbitrary signing, deadline, reader saturation and ORL1 tests remain passing |
| Production node construction | bin/outbe-chain/src/launch/node/radicle.rs:142 | all-consumers.log:7 packages all-target check; scoped-clippy.log adapter+node all-target -D warnings |

`checks.json` records exact argv/cwd/environment/toolchain/platform/times/exits/spec hashes and log hashes. All64 adapter tests passed/0ignored; all seven consumer package checks, scoped Clippy and touched-file rustfmt passed. `source-scope.json` records changed source hashes. `sha256.json` covers this bundle.

Not task acceptance: P3 remains in_progress, no acceptance reviewers or closure requested. Public DKG Output transfer, sidecar signer calls/collector consumption, automatic bootstrap, latest publication/promotion, Merge and full multi-validator production E2E remain incomplete. Existing full-node shutdown and cross-UID/Linux behavior were not exercised by this component run. D3 description1024→255 proposal remains unapproved; dependent bootstrap and all bounds/specs unchanged. Initial development fixture Vec→FixedBytes compile error was corrected before this frozen source; this bundle claims only the successful exact-SHA run.
