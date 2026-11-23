# Late Finalize Vote Retention: Threat Model

Status: deferred security debt for the branch-local late-credit accounting change. No exploit or related E2E failure has been observed. This document does not claim that the current `main` or live testnet contains the change.

## Normal behavior

A `Finalization` certificate for one proposal contains at most one signature from each committee member. A committee of 4 therefore contributes at most 4 signatures to one certificate; a committee of 128 contributes at most 128. A block digest can be proposed in more than one consensus view. Each `Finalize` signature covers the full `(epoch, view, parent_view, digest)` proposal, so signatures from different views cannot be combined into one certificate or late-credit aggregate.

The branch-local late-credit change buffers verified individual signatures for each full proposal binding. The proposer selects only signatures matching the parent proof escrowed on its candidate branch. This avoids losing late credit when a local direct finalization and the candidate branch use different valid views of the same block.

## Threat actor and path

The actor is a committee member that controls its own signing key. It cannot forge another member's signature. Commonware accepts at most one `Finalize` from that signer in a given view, but reports an admitted activity before requiring a match with the round's actual proposal. `FinalizeVerifyActor` verifies the individual signature over the supplied proposal and then forwards it to `LateFinalizeSigStore`. This is sufficient for a malicious member to submit a different signed binding for the same digest as successive views become admissible, even if no certificate forms for that binding.

The branch-local store limits the number of distinct digests to 512. It does not limit the number of `(epoch, view, parent_view, signer)` bindings for one digest. It also keeps pending bindings after local target resolution. If the network advances through `V` admissible views, one malicious signer can add at most one such entry per view for a chosen digest. With a Byzantine threshold of `f = floor((N - 1) / 3)`, the corresponding conditional bound is `f * V` entries for one digest: `V` at `N = 4`, or `42 * V` at `N = 128`. These are bounds over time, not signatures in one certificate or an instantaneous admission rate.

## Impact and current evidence

The new inner map can grow with the number of views over the process lifetime. Proposal assembly and local target resolution scan that map while holding its mutex. An extended malicious campaign can therefore increase memory use and processing time. Outer hash eviction and ingress rate limits slow or displace accumulation but do not impose a fixed per-digest, per-signer bound. This is a code-level resource-exhaustion risk, not evidence of an observed attack, an invalid block, or a current E2E failure. The impact and time to exhaustion have not been measured.

## Deferred mitigation and acceptance boundary

A future fix should bound retained full bindings per `(digest, signer)` across views and epochs, reject out-of-range signer indices before allocation, preserve exact branch binding, and never let one signer evict another signer's entries within a retained digest. The capacity and replacement rule are local best-effort late-credit policy choices; they are not consensus validity rules. Tests should cover repeated signed views, alternate valid finalization views, aggregate verification, and the memory bound. Hash-cache recency under adversarial churn needs a separate policy review.

Mock and mock-native E2E campaigns exercise the normal functional path. Passing them cannot establish resistance to this adversarial resource-exhaustion path. The final acceptance report must disclose this deferred risk if the branch-local accounting change remains in the final chain SHA.
