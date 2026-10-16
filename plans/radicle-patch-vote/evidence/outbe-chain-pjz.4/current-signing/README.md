# Intermediate P3/P6: current finalized signing permission

Source: `289d3a51e839ce6c10a6af5cdd71fb3e915096f8`. Exact commands, environment,
platform/toolchain, specification hashes, timestamps, exits and log hashes are in
`checks.json`. This is partial implementation evidence, not full task acceptance.

| Required fragment | Production source | Behavioral evidence in full-libs.log |
| --- | --- | --- |
| Current effective committee, Approved Vote, stage, network RID, bound material and latest receipt sampled from one exact canonical finalized state | `manager/snapshot.rs::read_current_patch_signing_state`, sealed `manager/types.rs::FinalizedPatchSigningState` | `current_patch_signing_state_uses_one_exact_finalized_state_and_preserves_consumed_permission` asserts exactly one state read and immutable approval after Reviewed |
| No pending/latest state fallback, invalid binding or future data | same reader, existing `RadicleRegistry::patch_approval` terminal Vote validation | `current_patch_signing_state_rejects_pending_expired_bad_binding_and_future_records` exercises pending Vote, expired Vote with stale approval record, unknown stage, absent material/receipt, wrong descriptor, future approval/receipt, missing finality, noncanonical head and missing exact state |
| Only checkpoint-derived native Review and derived signed refs cross the local signing mailbox | `integration/review.rs::CheckpointPreparedReview`, `CheckpointPreparedReviewRefs`; `integration/network.rs::EndpointNetworkResolver::{sign_review,sign_review_refs}` | `exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts` now obtains actual native contributions through four independently running local endpoint actors |
| Fresh approval/stage/checkpoint/material gate before each contribution | `integration/signing.rs::authorize_current_review`; `integration/network.rs::EndpointNetworkState::review_partial` | same native scenario invokes `network/material_tests.rs::signing_partials` for both subjects, checking every request rereads current state; consumed stage, changed checkpoint/approval, unavailable reader, wrong generation, withdrawn output/capability fail closed |
| Existing local Share remains behind provider; typed job returns only authenticated partial | `integration/material.rs::install_local_signer`, `engine/src/stack/epoch/run.rs` provider wiring, existing fixed-domain `VrfMaterialProvider::sign_repository` | actual SDK threshold contributions are serialized as 268-byte authenticated frames, verified and recovered; subsets 012/123 produce equal COB and refs signatures, OIDs and manifest; engine full regression suite included |

The engine installs an Arc-backed local provider capability in a separate in-process
watch channel. The public Output channel/value still contains public DKG data only.
No Share is extracted, serialized, sent to a peer or reconstructed. The node actor
uses its existing local MinPk identity key to authenticate a partial. Signing stays
outside consensus execution. The provider's existing active-version/full-polynomial
check remains under its mutex; the node first reads fresh finalized permission and
uses that same authority to construct RepositorySigningMaterial.

The production request has no peer-selected height, arbitrary message, domain or
namespace. The mailbox retains its existing bounded nonblocking admission and
deadline. This is a same-process typed bridge, **not** completed protected UDS.
No generic Git storage/fetch gate, D2 or changes to storage/git.rs were introduced.

## Evidence limits and remaining work

The Reth reader tests use exact-state providers backed by real Vote/registry state;
they do not run live block consensus. The native scenario uses actual BLS SDK shares,
local actors and native Git objects but fixture checkpoint/authority readers and a
manually accepted bootstrap receipt. The local state mutations are rejection probes,
not live rotation/quorum-publication evidence. No network transport is exercised by
the test-only NoSend endpoint transport. Existing engine tests are regression evidence.

Protected node/sidecar UDS, independent complete native-object and q-witness package
verification, distributed collector/journal, live freshness before publication,
automatic genesis bootstrap, separate Merge, managed second-node replication and
production multi-validator E2E remain incomplete. P2/P3 stay in_progress; no task
was closed and no acceptance reviewers/PASS are claimed.
