# Intermediate P3: authenticated finalized anchor admission

Source: `fe0411748f7146cd1b2e0d418be8d68c0947c63c`.
Specification hashes, exact commands, environment, times, exits and log hashes are
recorded in `checks.json`. This is intermediate evidence; P3 remains incomplete.

## Implemented path

`ApplicationShared::validate_verify_blocks` first checks the Simplex context
parent/height, then validates the publication anchor before header/DKG and Engine
execution admission. An enabled publication must name this chain/genesis and a
strictly earlier height. The genesis anchor is checked against the local trusted
genesis hash. Other anchors require the exact block from the local finalized
marshal archive, with one bounded height lookup. The general proposal cache is
not consulted for finality.

The pinned SDK (`commonware-consensus`, commit `d476a236`) implements
`marshal/core/mailbox.rs::get_block` as a best-effort local-storage request. In
`marshal/core/actor.rs`, `Message::GetBlock` with `BlockID::Height` calls
`get_finalized_block`; the digest variant has different semantics. The adapter
uses only the height variant and the existing verification-resolution timeout.

A wrong canonical hash or impossible height rejects the payload. Missing local
history, timeout or inconsistent returned height withholds the vote through the
existing local-unavailability path; these do not declare the payload permanently
invalid or stop the node. Disabled/legacy headers acquire no additional format
constraint from this publication-specific check. Node policy separately enforces
the genesis-selected OART version, and execution separately checks the exact
job-derived height, current committee, signatures and checkpoint transition.

## Behavioral coverage

- `publication_anchor_absence_and_exact_genesis_need_no_history_lookup`:
  absent publication and exact genesis; wrong genesis rejected; zero lookups.
- `publication_anchor_preserves_legacy_headers_and_rejects_malformed_enabled_frames`:
  opaque legacy and v0x0B unchanged; truncated enabled envelopes rejected.
- `publication_anchor_requires_the_exact_local_finalized_block_with_one_lookup`:
  exact non-genesis canonical hash, hash substitution, and one lookup irrespective
  of the height gap. This is a lookup test, not an independent fork-proof test.
- `publication_anchor_missing_or_inconsistent_local_history_is_unavailable`:
  absent block and wrong returned height are local unavailability.
- `publication_anchor_rejects_foreign_chain_or_non_ancestor_height_before_lookup`:
  foreign chain/genesis and equal/future height fail before storage access.
- `finalized_lookup_never_treats_a_proposal_cache_hit_as_finality`:
  a normal ancestry cache hit cannot satisfy the finalized-only adapter.
- `verify_prechecks_reject_or_withhold_before_engine_work`:
  actual application `handle_verify` rejects an impossible publication anchor;
  a parent present in the proposal cache but absent from marshal finalized storage
  withholds the response until cancellation. Neither reaches Engine API work.

The full consensus library suite also exercises the existing Simplex, DKG, VRF,
application, marshal, finalization and shutdown behavior.

## Boundaries

This change relies on the existing Simplex context-parent/chain safety contract;
it adds no independent ancestry walk for arbitrary candidate forks. The positive
anchor unit tests inject a finalized-reader fixture; they are not a live quorum
publication E2E. Collector transport/journal, current DKG output provisioning,
sidecar integration, finalized receipt policy, automatic native bootstrap and
complete native Review/refs/Merge publication remain separate unfinished parts
of the approved plan. No task closure or independent acceptance PASS is claimed.
There are no `git.rs`, native fetch-gate or D2 changes in this source.
