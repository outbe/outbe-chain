# Intermediate P3/P6: exact native Review package verification

Frozen source: `57a4aac883529227227315c148d2589df32c1eef`. Exact commands,
cwd/env/toolchain/platform, source/spec hashes, timestamps, exits and log hashes
are in checks.json. All 50 adapter tests, seven consumers all-target checks,
adapter Clippy -D warnings and scoped rustfmt passed. This is intermediate evidence,
not full P3/P6 acceptance or live end-to-end publication.

| Contract fragment | Production source | Behavioral evidence |
| --- | --- | --- |
| Missing pinned code/object dependencies prevent the typed signing job | `integration/review.rs::prepare_patch_review_with_objects`, public `prepare_patch_review_from_checkpoint` | native scenario copies actual ODB objects to a separate storage except approved head README blob; typed preparation and package verification fail; supplying that blob permits verification |
| Exact deterministic native Review, complete owner refs and sigrefs from current approval/checkpoint | `integration/publication/review.rs::prepare_review_publication` | actual native scenario compares reconstructed body/manifest with independently assembled existing native fixture; later author updates do not alter approved preparation |
| Verify complete received native package, q witnesses and exact manifest/body rather than peer-declared subjects | `verify_review_publication`, existing native attach/load/refs verifier, `RepositoryWitnessBundle::verify` | separate receiver storage verifies both 012/123 witness bundles using public-only material, producing the same expected manifest; no refs installed/promoted |
| Exact object type/OID/SHA256 and no extra reachable objects/ref changes | deterministic object inventory and full manifest comparison | wrong object kind/SHA256, removed required object, added already pinned base or unapproved later head, wrong old sigrefs and substituted owner main snapshot rejected even with recomputed manifest/body hash |
| Fresh permission and job-derived checkpoint/anchor before attestation | sealed `VerifiedReviewPublication::authorize_current`, private `review_body` | consumed stage, changed approval timestamp, advanced checkpoint and stale authority observation rejected; fresh material on newer observation head preserves the same package; wrong sequence/anchor height/hash rejected |
| Validate pinned Git content by OID, respecting external gitlinks | `integration/review.rs::object_closure` | `pinned_git_closure_checks_code_objects_and_preserves_external_submodules` checks commit/tree/blob closure, valid missing external gitlink and missing required root |

The native Review is reconstructed using the existing checkpoint-bound preparation
and native attachment APIs. Those APIs reload/check the actual stored native COB
and signed refs. Recovered envelopes are verified against reconstructed subjects;
all current q partial witnesses are independently checked. The receiver never
uses envelope fields to choose the expected Review action, metadata or refs.

The exact pinned forest consists of accepted complete owner refs and previous
sigrefs, plus approved revision/prior owner COB/identity and exact code base/head/
expected-target roots. All its local Git objects are checked for type/OID integrity.
The new inventory contains only the generated COB/sigrefs commits and recursive
trees/blobs, excludes already pinned dependencies, and never walks their parents
as newly created objects. Object commitments use SHA256 of raw content. Extra local
objects and later author tips cannot expand the package. External submodule gitlink
IDs are references to another repository, not missing objects of this repository.

The body derives sequence/previous receipt, descriptor/owner/network/material,
exact proposal/patch/revision/precondition and canonical job anchor from trusted
inputs. The native manifest is canonical and uses the fixed actor/time/messages.
The sealed proof has no Deserialize/peer constructor; its fresh guard compares the
same immutable approval, current Pending stage, exact predecessor checkpoint and
current authority/material before a later publication attestation.

## Evidence boundaries and remaining work

The independent receiver is a distinct temporary Git storage with a different Ed
transport identity, initialized by copying actual ODB objects. It does not hold a
BLS owner secret or signing Share. This is actual native package verification, not
normal network replication or a production two-node E2E. Its current authority,
bootstrap receipt and history policy are test providers; no live q-identity
checkpoint is executed by this scenario. The fixture later manually promotes its
already accepted source Review; this verifier only writes immutable objects and
leaves refs unchanged on both success and failure.

Git closure walks/reconstruction are synchronous and must run on the collector's
blocking worker outside consensus/endpoint reactor. Cost scales with pinned graph
size; production queue/worker/cancellation and liveness budgets remain unmeasured.
No new storage/git.rs/fetch admission gate, D2 or Heartwood Vote interpretation.

The publication attestation actor/guard wiring, protected node/sidecar UDS,
distributed collector/journal, automatic genesis bootstrap, BindMaterial/live
rotation scheduling, separate Merge and managed normal replication/production E2E
remain incomplete. Bootstrap/Merge package verification is not claimed by this
Review-only implementation. P2/P3 remain in_progress, P4/P6/P7/P8 open; no acceptance
reviewers/PASS or task closures were requested.
