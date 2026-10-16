# Accepted native Review installation — intermediate P6

Source `3632c4703586c2be486c4c15d68ef1f13faa110d`.
All 157 affected SDK/runtime tests passed, zero failed or ignored. Seven consumer
all-target builds, SDK/runtime/node Clippy with warnings denied, and changed-file
formatting passed. `checks.json` records exact source/base/spec hashes, commands,
cwd, environment/toolchain/platform, UTC, exits and hashes of the raw logs.
Frozen source remained unchanged. No task closure or full-AC acceptance is asserted.

## Change and proof

`crates/blockchain/radicle/src/integration/publication/review.rs` exposes native
ref installation only from `AcceptedReviewPublication`, whose constructor requires
a trusted accepted receipt, native reconstruction and a complete indexed q-proof.
`integration/publication.rs::install_owner_refs` is the shared Bootstrap/Review
predecessor-transition installer. It locks the complete desired owner snapshot and
signed-refs ref, checks unchanged refs and every changed old-or-new target, rejects
unexpected owner refs, then applies pending changes. Unchanged symbolic identity
refs remain symbolic. Other owners' namespaces are untouched. Bootstrap retains its
existing error mapping and canonical identity/default-branch setup. No native
`storage/git.rs` change was made.

The existing genuine native/threshold fixture
`integration/review/tests.rs::exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts`
now installs each independently verified accepted witness on a separate native
repository after the signing provider has already moved to different material.
It proves conflicting target, extra owner refs, conflicting patch COB and conflicting
signed-refs targets reject without other ref changes; clean install, retry and
recovery with COB already new but signed refs still old succeed. Native evaluation
shows Accept only for the exact approved revision, a later ordinary author revision
remains unreviewed, the patch stays open, and main remains at its prior head.
Ordinary author refs and the native symbolic identity ref survive installation.
Existing Bootstrap interrupted/repeated/conflicting installation and actual
three-sidecar bootstrap/restart/Noise/fetch regression remain green.

## Remaining contract

This is an installation primitive for a verified predecessor transition, not the
complete runtime Promote gate. The runtime must still bind promotion to its latest
trusted checkpoint, maintain monotonic accepted sequence, reconstruct historical
owner grants, and support fresh/lagging latest-snapshot installation without replaying
all intermediate local ref deltas. Automatic approved-job discovery, durable distributed
Review scheduling, checkpoint acceptance/installation/announcement integration,
archive delivery, BindMaterial, separate Merge and production E2E remain incomplete.
The DKG and accepted approval/receipt sources in this fixture are test fixtures;
real native objects, crypto and local storage are used. Three full-AC independent
reviews have not been requested and P6 remains open. No push/PR/deployment.
