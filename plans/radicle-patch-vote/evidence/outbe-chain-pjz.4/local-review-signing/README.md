# Fixed local Review signing — intermediate P3/P6 evidence

Source `0ff4183ba7ad059971b2fa35f8bc5462be0be088`, base `a6965c184`.
All 157 affected SDK/runtime tests passed, with zero ignored; seven consumers built
on all targets; SDK/runtime/node all-target Clippy with warnings denied and all
eleven changed Rust files formatting passed. `checks.json` records full source/base
and spec hashes, exact argv/cwd, Rust/platform, environment, UTC, exits and raw log
hashes. Source was unchanged during verification. This is not full-AC acceptance.

## Required behavior, source and proof

| Behavior | Source | Evidence |
| --- | --- | --- |
| Fixed local Review COB and refs jobs; no arbitrary-byte signing | SDK `integration/local/review.rs:65`, `:187`, `:202`; dispatch in `integration/local.rs` | Actual Unix socket/client/server/endpoint actor in `integration/bootstrap/tests/local_review.rs:64` |
| Node reads its own finalized permission and accepted preparation snapshot | `integration/local/preparation.rs::current` and `local/review.rs::sign` | Caller uses cached exact job; replacing the reader's approved revision makes the server reject it; unavailable current permission rejects |
| Native bytes rebuilt by node before share use | `prepare_patch_review_from_checkpoint`; fresh actor authorization remains in `network.rs::review_partial` | Ordinary Ed author creates patch before network Review; later unapproved revision does not change the accepted job; one local partial plus two genuine other partials recovers the exact native COB signature |
| Signature for refs binds the recovered Review COB | `local/review.rs::sign` reconstructs and attaches the supplied envelope before preparing refs | Correct native COB envelope yields a verified refs partial; valid bootstrap envelope substituted for it rejects |
| Client independently verifies participant, identity signature, native subject and polynomial | `integration/signing.rs:324`, `:342` | Both returned partials verified using a public-only provider; provider absent on the node initially rejects |
| Fresh process reads network identity through an accepted bootstrap proof | `integration/publication/store.rs:353` | Server opens its own Storage handle and rebuilds the existing generic policy from own receipt and archived full native/q proof; native Review preparation then succeeds |
| Descriptor/content inputs are part of a durable complete bootstrap package | `store.rs:246`, `:331`; `bootstrap.rs::VerifiedBootstrapPublication`; runtime accepted install reads the archive | Reopen loads exact inputs; altered descriptor/content and missing content reject; repaired exact bytes load. The full automatic three-sidecar/restart/fetch test passes with this archive consumer |
| Node transport identity is only public read metadata | `local.rs:213`, launcher `bin/outbe-chain/src/launch/node/radicle.rs:154` | Configured from existing preflight NodeId; no profile secret, BLS share export or owner delegation API added |
| Malformed local requests reject | `local/review.rs:232` and existing protected socket bounds | Unsupported step, short/trailing data, relative path, missing required signature and missing node metadata reject; wrong client genesis rejects |
| Signing does not publish Review or change owner refs | Existing native preparation APIs; no install called by Review signer | Exact complete owner refs including sigrefs remain at the accepted checkpoint after positive and negative requests |

SDK paths in the table are relative to `crates/blockchain/radicle/src/`.
The archived root helper grants only entries proven by the original accepted
bootstrap. It does not authorize a new Review, change branch heads, or grant a user
the network owner's permissions. Every actual partial still requires the actor's
own current finalized approval and material/provider checks.

## Diagnosed before freezing

A provisional new refs command initially reused existing `OCS1`. The unchanged
`local_signing_state_is_requested_fresh_for_the_exact_proposal` regression failed
both in the complete lib suite and alone. The new refs command became `OCV1`;
existing `OCS1` state bytes were preserved. Both that regression and the new Review
path pass in the frozen full run. Earlier diagnostic runs were uncommitted; their
output is not represented as exact-SHA acceptance evidence.

## Remaining requirements

- The Review test uses fixture finalized approval/receipt readers and centralized
  test DKG material. It is a real native Git/crypto/UDS/actor path, not production
  EVM Vote/finality or the P8 end-to-end scenario.
- The automatic worker still schedules only bootstrap. Approved-job discovery,
  distributed Review collection/publication, accepted Review promotion, separate
  Merge and BindMaterial runtime integration remain incomplete.
- Root read-policy reconstruction is implemented here; complete reconstruction
  of all later accepted owner-entry grants is still required for full replay.
- New-node archived package delivery, nonempty genesis artifact distribution,
  full rotation/window/budget/crash/liveness tests and production E2E remain.
- Full-AC evidence and three independent per-task/final reviewers remain required;
  no implementation stage or review was closed. No native `storage/git.rs` change,
  push, PR or deployment.
