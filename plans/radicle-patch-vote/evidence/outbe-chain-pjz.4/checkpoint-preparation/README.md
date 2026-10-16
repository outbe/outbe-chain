# Intermediate P3/P6 prerequisite: checkpoint-pinned Review preparation

Source: `48a30d859`. The full SHA, parent, specification hashes, exact commands,
environment, times, exits and log hashes are in `checks.json`. This is partial
implementation evidence, not full P3/P6 acceptance or a task closure.

## Implemented boundaries

| Contract fragment | Source | Behavioral evidence |
| --- | --- | --- |
| Manifest V1 canonical JSON, private decoded representation, fixed fields and arrays, one MiB input cap | `integration/publication.rs::PublicationManifest` | `canonical_manifest_has_exact_sorted_wire_bytes_and_rejects_noncanonical_inputs`, `manifest_rejects_duplicates_bad_hex_refs_subjects_and_silent_unicode_changes`, `manifest_enforces_the_exact_one_mib_limit_before_raw_decode` |
| Receipt from exact local canonical finalized state, bounded stored body decode, no latest/pending fallback | `manager/snapshot.rs::RethSnapshotReader::read_publication_receipt`, sealed `FinalizedPublicationReceipt` | both `publication_receipt_reader_*` tests |
| Replay exact committed owner refs and sigrefs, sequence/hash CAS, unchanged owner/chain/RID/descriptor, BindMaterial preserving native state | `integration/publication.rs::CheckpointOwnerSnapshot` | `checkpoint_snapshot_replays_bind_material_without_losing_the_native_sigrefs`, `checkpoint_snapshot_rejects_wrong_receipt_job_subject_owner_and_partial_refs`; actual native Review replay and failed-CAS atomicity in the native scenario |
| Review resource, previous owner COB tip, full owner refs and previous sigrefs pinned to the checkpoint | `integration/review.rs::prepare_patch_review_from_checkpoint`, `CheckpointPreparedReview::attach` | extended `exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts` |

The manifest uses Heartwood's existing `CanonicalFormatter`; its module was made
public with no formatter/Doc encoding changes. Provenance records this generic
API visibility change. Heartwood does not interpret Vote or receipts.

The parser rejects duplicate/unknown JSON fields, extra whitespace/keys, wrong
version, unsorted input arrays, repeated OIDs/names/ordinals, bad lower-case hex,
invalid/namespace-prefixed ref names, virtual/self sigrefs in the owner snapshot,
unchanged ref deltas and malformed native envelopes. Typed construction sorts
arrays and rejects duplicates. Git ref names are never silently normalized by
canonical JSON. The exact one MiB limit and limit+1 are exercised. Interchangeable
partial witnesses are not part of this JSON schema or its hash.

`CheckpointOwnerSnapshot` binds the manifest hash, native subject kinds, owner,
repo and OPJ1 context fields to a finalized receipt. It applies complete ref
deltas to a copy, checks the old OIDs and the complete resulting owner snapshot,
then advances state. BindMaterial requires empty native arrays and an unchanged
owner snapshot; multiple such checkpoints retain the last native sigrefs OID.
This supports incremental journal replay without treating each material binding
as a new native refs write.

## Native scenario

The existing four-player threshold scenario now builds native bootstrap fixture
objects with exact OPJ1 contexts, pins their refs through a manifest/receipt
fixture, and prepares the approved Review through the checkpoint-bound API.
It rejects mismatched chain/genesis/epoch/version/material/previous receipt.
An additional unapproved author revision does not change the prepared Review.

Subsets 012 and 123 recover equal COB and signed-refs envelopes. Independent
attachment of those preparations produces identical COB and sigrefs OIDs. Their
canonical Review manifests are byte-identical. The test inventory traverses the
specific native commit trees and excludes pinned dependency OIDs; it does not
walk arbitrary local author tips. The complete desired owner snapshot preserves
main and identity and updates the exact patch COB. Applying the receipt advances
the owner snapshot; a wrong old sigrefs OID fails without partial state changes.
The prior signing context cannot prepare a new action from the advanced snapshot.
Attachment itself does not promote refs. Native loading still shows Accept only
on the approved revision and the patch remains open.

This scenario constructs its bootstrap/receipt provider locally. It is not an
automatic genesis bootstrap or an executed q-identity-attestation checkpoint.
The separate parser fixtures have real threshold signatures over synthetic native
subjects; they are codec/replay tests, not complete native Git package proofs.

## Outstanding claims

Manifest validation and `CheckpointOwnerSnapshot` pin commitments; they are
**not** the independent native-object/SHA256/envelope/q-partial-witness verifier.
That verifier must validate the actual objects and witness bundle before package
attestation/publication. The future signing actor must also reread current exact
Approved permission, execution stage, current material and latest checkpoint
before issuing a partial; this preparation API alone does not grant permission.

Protected local signer transport, distributed collector, complete package and
journal/crash flow, automatic bootstrap, normal announcement/second-node managed
replication, separate Merge and full production E2E remain unfinished. No task
acceptance reviewers were requested and no independent PASS is claimed. No
`git.rs`, native fetch-gate or D2 changes were made.
