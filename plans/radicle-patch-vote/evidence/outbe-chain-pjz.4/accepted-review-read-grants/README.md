# Accepted Review read grants for the local signer — intermediate P3/P6

Source `d92efe0ceff4644a0b769630f80287448b845749`.
All 158 affected SDK/runtime tests passed, zero failed or ignored. Seven consumer
all-target builds, SDK/runtime/node Clippy with warnings denied, and all changed
Rust files formatting passed. `checks.json` records exact source/base/spec hashes,
commands/cwd, environment/toolchain/platform, UTC, exits and raw log hashes.
The frozen source remained unchanged. No full-AC acceptance or closure is asserted.

## Defect, correction and callers

The local Review signer and complete-package verifier previously reopened native
storage with only accepted Bootstrap read grants. A following Review requires the
previous accepted owner COB tip and signed-refs commit; the root-only policy denies
those entries. This happens when opening storage for a request, without requiring
a process restart. Vote records and accepted decisions were never lost.

`integration/publication/store.rs::open_checkpoint_repository` replaces the root-only
opener. It verifies root receipt/package/native/q-proof, then replays accepted Review
receipts through the requested checkpoint. Each Review obtains its immutable exact
approval from the own-chain reader, verifies the complete native package and q-proof
against the predecessor, adds only sealed historical read grants, and advances the
accepted preparation snapshot. Target receipt equality and sequence continuity are
checked. Neither refs nor current signing permissions are changed.

`integration/local/review.rs::{sign,publish}` both pass their already sampled
current signing state's checkpoint. The actor still repeats current permission and
material checks before using its own existing provider share or attesting/queuing.
`RepositoryReceiptReader::read_current_patch_approval` is implemented by the existing
Reth exact-finality approval reader. Unavailable implementations fail closed.
All source paths above are under `crates/blockchain/radicle/src/`.

## Behavioral proof

- The actual Bootstrap/Review UDS fixture persists a real complete q3 package, stops
  its actor/socket, reopens the package store, and opens native storage with new grants.
- A package without an own accepted receipt rejects. A receipt without its exact
  approval rejects; changing the approval timestamp also rejects.
- With the genuine accepted receipt/approval, reopening succeeds even when the
  current signing permission reader is unavailable. Native Review entry loading
  and accepted signed-refs loading succeed. The same target next Review fails on
  the old root-only handle and prepares successfully with reconstructed grants.
- All owner refs remain exactly unchanged throughout. No repeat Vote, signing of an
  already accepted decision, or native ref promotion is performed by this opener.
- `archive_reader_uses_current_finality_without_revoking_an_accepted_vote` verifies
  exact own finalized state reads, immutable approval after membership changes,
  and failure without finality. Existing current signer/material/queue rejection
  cases and automatic three-sidecar Bootstrap regression remain green.

## Remaining work

This replay currently handles Bootstrap and Review only; BindMaterial/Merge remain
unimplemented and reject. Full approved-Vote-to-automatic-Review scheduling, durable
distributed Review collection, latest-checkpoint/monotonic promotion and fresh/lagging
snapshot installation, public archive delivery, separate Merge, rotation/window
scenarios and production E2E remain incomplete. DKG and finalized readers in the
local fixture are fixtures; native objects/crypto/socket/actor/storage are real.
Three independent full-AC reviews have not been requested. P3/P6 remain open.
No native `storage/git.rs` edit, push, PR or deployment.
