# Local complete Review publication bridge — intermediate P3/P6

Source `48a542fc3cd5027d6529e7cb68279f9a4261572b`. Full 157 affected SDK/runtime
tests passed, zero ignored; seven consumers all-target builds, SDK/runtime/node
all-target Clippy with warnings denied and all seven changed Rust files formatting
passed. `checks.json` records full source/base/spec hashes, exact argv/cwd, Rust and
platform, environment, UTC, exits and raw log hashes. Frozen source did not change.
This is intermediate evidence; no full-AC task acceptance or closure is asserted.

## Implemented path

The existing protected OCA1/OCQ1 complete-package transport now dispatches on the
canonical publication body's action. Bootstrap keeps its existing wire bytes.
Review uses the same bounded path/body/manifest/witness/batch frame; the reserved
content-artifact OID is zero. Unsupported actions and a nonzero Review artifact
field reject. Existing same-UID/private-socket, reader, deadline and frame bounds
remain in force.

`integration/local/review.rs::publish` obtains this node's current finalized
permission, material and accepted preparation snapshot. A blocking worker opens the
native repository with its own accepted root read policy, reconstructs the complete
native Review package and validates every indexed q-witness. Every submitted batch
identity signature is checked before durable selection. The node's existing
`ReviewPackageStore` persists the full immutable package/witness and selects the
attestation slot before the actor can attest or queue it.

The actor's `review_publication_material` repeats own-current chain, exact approval,
execution stage, predecessor and authenticated material checks. `queue_review_publication`
checks the exact body, bounded batch encoding and every current identity attestation
before updating the existing one-batch mailbox. Bootstrap and Review share an enum
holding one selected publication token. Material changes, finalized refresh and
shutdown clear stale ready data. Proposer/consensus code and the batch32 carrier are
unchanged; Git, witness verification and persistence remain outside them.

## Behavioral proof and caller map

- `integration/local.rs` dispatches complete packages to bootstrap or Review from
  their typed body action. Existing bootstrap UDS tests and the automatic actual
  three-sidecar/restart/Noise/fetch test remain green.
- `integration/local/publication.rs::Request::parse` shares canonical parsing and
  size/path/witness/batch checks. Its request encoder is used by both bootstrap
  clients and the new Review clients; existing raw-frame bootstrap test caller was
  updated to the explicit body/manifest arguments without changing frame layout.
- `LocalReceiptClient::{attest_review_publication,queue_review_publication}` use
  this transport. The client independently verifies the node's returned MinPk
  attestation against the exact body/current public material.
- `integration/bootstrap/tests/local_review.rs::assert_review_socket`, called from
  the native bootstrap test, creates a genuine ordinary Ed patch and exact approved
  revision. Actual SDK sockets and actor produce a COB partial, public q3 recovery,
  refs partial and public q3 refs recovery. The resulting real native package is
  sent through the new complete Review path.
- q-1 native witnesses reject despite an unchanged valid manifest. An invalid final
  batch identity signature also rejects. Both cases leave the Review slot absent
  on disk and the ready mailbox empty, proving validation precedes selection.
- A valid package creates the durable slot, returns the expected current index0
  attestation, and returns the identical attestation on retry. The ready mailbox
  contains the exact three-entry batch; a smaller payload budget reads its fitting
  prefix without consuming the full batch.
- Withdrawing material clears ready data and prevents attestation. Restoring the
  matching material permits retry. Replacing the reader's approved revision rejects
  cached package/queue requests; finalized refresh clears ready data. A valid batch
  is queued again before shutdown, and shutdown clears it.
- Native owner refs, including sigrefs, remain exactly at the previous accepted
  checkpoint. Neither local attestation nor a ready block batch promotes Review.

All SDK paths are relative to `crates/blockchain/radicle/src/`.

## Remaining work and proof boundaries

The DKG and finalized approval/receipt readers are fixtures. Native objects, BLS
partials/recovery, protected Unix sockets, SDK actor, durable package store and
ready mailbox are real. This does not prove production EVM Vote/consensus execution
or the final production E2E.

The automatic sidecar worker still schedules bootstrap only. Approved-job discovery,
distributed durable Review collection, actual checkpoint acceptance and native
accepted Review installation/announcement remain incomplete. Full later-history
grant reconstruction, archived package delivery to fresh nodes, BindMaterial,
separate Merge, rotation/window/budget/liveness/crash scenarios and all required
three independent full-AC reviews also remain. P2/P3/P4/P6 and the feature stay open.
No native `storage/git.rs` changes, push, PR or deployment.
