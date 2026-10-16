# D1 — Reachable owner admission and cache provenance

Status: PROPOSED. Not implemented or approved. Requires three independent decision checks, then explicit user approval before dependent implementation. This is an implementation choice inside existing P2, not a change to P0 contracts or new feature scope.

## Concrete problem

The current unfinished entry-level policy hook rejects a crypto-valid managed-owner COB when loaded directly. An Ed25519 entry can still reference that owner entry as Resource or Related and pass `Repository::load` and `Op::load`. The actual Git parent is present, but the owner policy is not consulted for the descendant. This violates design.md §1 and P2's existing requirement to reject dependent updates even through another namespace. A pure Ed entry must remain allowed before Vote.

Production sources:

- `radicle/src/storage/git/cob.rs`: new `load_entry` authorizes only the current BLS entry; the Ed return path does not inspect its ancestors.
- `radicle-cob/src/backend/git/change.rs`: `load_entry` removes Resource/Related from `Entry.parents`; those are nevertheless actual Git parents.
- `radicle-cob/src/change_graph.rs`: `load` walks `change.parents` only and logs load errors; evaluation prunes invalid branches. This is legacy best-effort graph behavior, not a complete managed-owner admission predicate.
- `radicle/src/cob/stream/iter.rs`: operations are filtered by manifest and loaded through raw Git, which has no trusted repository policy.
- `radicle/src/cob/patch/cache.rs`: StoreReader `get/find_by_revision/list/list_by_status/counts` directly query SQLite. No Git/policy check or receipt provenance exists in that path. Issue cache has an analogous materialized read boundary and must be accounted for where managed BLS entries can reach it.

`repro/` is an actual standalone executable using current production storage/load/op APIs, real Git objects, the pinned Commonware threshold native COB fixture, and an explicitly denying test policy. It asserts direct owner denial, pure Ed admission, then observes inappropriate admission of Ed Resource/Related descendants. Its successful exit means the counterexample was reproduced, NOT that the security contract passed. It does not claim an end-to-end merged-patch exploit, current-validator proof, or a completed P2. Cache bypass is source-backed here; no cache runtime exploit is claimed.

## Proposed option A (recommended for independent assessment)

Keep native entry signature verification and add one shared policy-aware admission of each candidate tip's complete reachable Git closure, including Resource/Related and other COB types, before using that tip. It recognizes actual owner entries and verifies their exact native bytes/accepted receipts; no receipt or context is inferred from an envelope or delivering namespace. Direct exact lookups and stream reads use the same predicate.

Admission is per candidate tip/branch. A denied owner entry quarantines that entry and its dependents, including an Ed descendant. It must not deny the entire repository or suppress independent pure Ed tips/revisions. Evaluators receive admitted tips only; legacy graph best-effort rules do not turn policy denial into a disconnected valid descendant.

A cached materialized view binds the exact admitted source tips and owner receipt provenance used to create it. On get/find/list/count, and reopen/rebuild, revalidate that provenance and current source view through the same admission boundary. Missing/stale provenance in a managed view requires rebuild through admitted Git history or a clear denial if unavailable; SQL state alone is not authority. Preserve existing normal Ed/static native behavior and data; do not wipe all caches or disable caching globally.

Existing publication promotion/finality/CAS requirements remain mandatory, separate from this read admission decision. This proposal does not replace them with historical-entry checks. Outbe remains responsible for trusted receipt/freshness inputs. Generic Heartwood does not interpret Vote, epochs, or validator roles.

Expected implementation map: `radicle/src/storage/{publication.rs,git.rs,git/cob.rs}`, `radicle/src/cob/{store.rs,stream.rs,stream/iter.rs,patch/cache.rs,issue/cache.rs}`, affected cache migrations/queries/readers and callers. Fetch/local install must call the same admission predicate at their existing validation/promotion boundaries. Reuse existing native verification/load helpers and storage interfaces. Confirm actual callers before editing. Exact Rust method names are implementation details; no new protocol/signing format is proposed.

## Alternative B

Propagate typed authorization failures through generic `radicle-cob::Storage`/ChangeGraph and explicitly prune all dependent branches, extending traversal to Resource/Related. Cache/direct/stream admission and provenance still need wiring. This changes more generic graph/error semantics; it may be preferable if source inspection shows a substantially smaller correct implementation. Do not silently select it or change accepted behavior without user approval.

## Required proof for either option

1. Crypto-valid denied owner entry is denied directly and through Ed Resource, Related, ordinary parents, mixed types, nested ancestors and another namespace.
2. Independent pure Ed patch/revision tips and exact lookup remain available before Vote, even alongside an invalid dependent branch.
3. Managed provider missing/unknown/denying, forged request/context/receipt and stale provenance fail closed; static BLS and Ed native paths remain valid.
4. Live Git evaluator/direct/stream and SQLite get/find/list/count/reopen/rebuild agree; warming a cache cannot launder an owner entry.
5. Old cached rows without required provenance cannot bypass policy; permitted rows rebuild without destructive global cache reset.
6. Preserve receiver/latest snapshot/CAS/history semantics from design.md; archive permission is not new canonical promotion permission.
7. Existing native suites/root consumers and actual two-peer BLS scenario remain required before P2 acceptance. This decision review does not close any implementation or acceptance-review task.

## Review request

Each of three independent non-author reviewers must inspect the same source snapshot and proposal, reproduce the counterexample in its own log, judge whether the problem is real, whether A is feasible/within scope and preferable to B, and identify concrete required corrections. Do not read peers' reports before submitting. Do not edit implementation/spec/proposal or close Beads tasks. Write only own D1 report/logs. The coordinator then presents all three conclusions to the user and waits. A review PASS does not authorize implementation.
