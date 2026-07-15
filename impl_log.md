# Implementation log

## 2026-07-16 — ADR-006

- Began with the canonical `EntityId36` seam in a new `outbe-compressed-entities` system crate. This keeps the WWD BE4 prefix and complete 32-byte digest in one fixed type before changing domain APIs, preventing another U256 surrogate from leaking into the reset design.
- Added a behavioral test for exact byte layout, round-trip parsing, and rejection of non-36-byte inputs. The test exercises the public type rather than inspecting source text.
- Published the normative v1 `.proto` messages and started the strict-codec tracer with Tribute: the test fixes exact canonical payload bytes, envelope round-trip behavior, and rejection of an unknown field.
- Added the Nod item canonical-wire tracer next, keeping the codec work vertical: exact payload bytes and semantic round-trip are specified before its implementation.
- Added the third canonical-wire tracer for Nod bucket bodies, including the exact B256 key and fixed-width amount encodings.
- Started CES1 commitment work across every ADR-specified 31-byte chunk boundary and pinned the resulting values in a checked-in interoperability fixture produced with Noble's generic Poseidon permutation and the protocol's pinned Circom parameters.
- Added the leaf tracer: owner/day Poseidon identity, identity-field conversion, and the final leaf must bind scheme, schema, identity, payload length, and payload bytes; unsupported fork versions are rejected.
- Added strict typed-envelope cases for inactive schema versions and alternative Protobuf encodings (unknown/duplicate fields, non-minimal varints, explicit defaults, empty payload, and schema zero).
- Started the ADR-006 direct-map backend with a behavioral test proving zero-as-absence and isolation of Tribute, Nod item, and Nod bucket namespaces even for the same EntityId36.
- Migrated production Nod reads and mutations to authenticate item and bucket bodies against the direct EVM commitment maps. Issuance, removal, and qualification now update the commitment and emit the exact previous/new leaf plus canonical payload in the same storage journal.
- Kept the bucket qualification worklist compact by persisting only the reversible WWD prefix beside each bucket key; the hook reconstructs the 36-byte bucket identity and verifies MongoDB before changing qualification.
- Removed the active NodFactory `uint256` ID boundary: issue/mining paths and semantic events now carry `EntityId36`/Solidity `bytes`, while PoW hashes the exact lowercase hexadecimal 36-byte identity.
- Migrated Tribute and Nod repositories to strict canonical `StoredBody` bytes and exact 36-byte primary/index/cursor identities; Memory and Mongo share the same repository contract and corruption classification.
- Reworked finalized projection decoding to validate scheme/schema, exact event identity, canonical payload, recomputed new leaf, and ordered previous/new transitions before any block domain write. Replay permits an unknown first transition after a partially committed receipt, then enforces continuity through the in-memory block overlay; the checkpoint remains last.
- Added pinned CES1 golden values for every ADR-specified `PBytes` chunk boundary, owner/day identity, identity field, and all three v1 body leaves, generated independently with `@noble/curves` and stored in `vectors/ces1-noble-poseidon.json`, plus canonical BN254 rejection cases.
- Added raw ABI-head preflight for every public `bytes` entity-ID input so non-36-byte IDs fail before the allocating Alloy decoder and before repository/state reads.
- Replaced stale Nod tests with behavior-level tests over canonical events, repositories, verified execution reads, commitment mappings, and journal rollback; no test inspects source text.
- Made Tribute and Nod aggregate counters fail closed on overflow/underflow instead of saturating into a different committed body.
- The host now independently recomputes and validates the enclave's Tribute `Poseidon(owner, worldwide_day)` digest before constructing `EntityId36`.
- Projection persists the exact canonical payload carried by each receipt instead of reconstructing it from decoded domain fields, so byte identity survives event-to-Mongo replay.
- The crash matrix now spans four successive receipts plus checkpoint failure and proves convergence after every injected commit boundary.
- Canonical identity validation is uniform at execution, projection, and repository boundaries: `EntityId36` must equal `WWD_BE4 || Poseidon(owner, WWD)`.
- Removed readerless production dispatch traps and preserved fail-closed behavior only at the point where a committed body is actually required.
- Added the EE0D account marker used by EVM state conversion so all three commitment mappings survive pruning and journal rollback tests.
- Hardened projection planners so an exact canonical `StoredBody` is the only Store input; the semantic body and every secondary index are derived by strict decoding inside the planner, eliminating payload/index divergence.
- Expanded the external Noble fixture to cover every CES1 tag, all three identity forms, all three v1 payload/envelope/leaf combinations, schema variation, bit flips, and rejection inputs; Rust behavior tests consume the artifact instead of embedding leaf outputs.
- Added table-driven execution-read tests that independently mutate every Tribute, Nod item, and Nod bucket field plus schema, envelope payload, and EVM leaf and require corruption before domain use.
- Added rollback coverage for Tribute and Nod item/bucket commitments, compact state, worklists, and events under nested revert and out-of-gas outcomes.
- Added a first-executable-block replay test that projects receipts emitted by real Tribute/Nod execution and matches all three Mongo bodies against the resulting three EVM commitment namespaces.

### ADR-006 verification boundary

- `cargo test` passes for compressed-entities, offchain-data, Tribute, Nod, both factories, Metadosis, EVM library, and the EVM subcall integration.
- `cargo check --workspace --all-targets`, formatting, and `git diff --check` pass.
- The only known full-workspace failures are the two Lysis cases that require multiple same-block mutations of one Nod bucket. They exercise the permanent journaled body overlay introduced by ADR-007 and are intentionally resolved in that next separate commit.
- Tribute and Nod item are immutable through their ADR-006 domain APIs, so their update replay fixtures exercise the exact journaled commitment/event transition directly; real cross-domain `update` capabilities, transaction-receipt parity, and failed-transaction coverage are completed through ADR-007's generic lifecycle rather than adding test-only domain mutators here.

## 2026-07-16 — ADR-007

- Started from the existing `outbe-compressed-entities` seam rather than adding a parallel lifecycle layer: the module will own generic existence transitions, opaque verified-body capabilities, block overlay/index deltas, canonical body events, and cleanup while Tribute/Nod retain business state and product events.
- Reproduced the two pre-ADR-007 Lysis failures: repeated same-block Nod issuance sees a non-zero bucket commitment but cannot find the unfinalized bucket body in MongoDB. These are the first red tests for overlay-first `Set` reads.
- Identified two executor requirements that must be explicit in this commit: an execution-scoped phase capability is needed to reject access after cleanup without adding an undocumented consensus slot, and body-mutating hooks must participate in receipt-visible gas accounting before state-root finalization.
- Added the fixed EE0D schema-v1 overlay layout at slots 0–10, including reversible body identity records, full canonical pending bodies, query-index delta records, and deterministic first-touch lists. Direct commitment writes remain private behind the generic lifecycle.
- Added the closed `EntityRef`/`BodyInput`/`QueryRef` API, opaque `VerifiedBody` value capability, and consumer-owned `ParentBodySource`; callers cannot select raw collection IDs, commitments, schema versions, emitters, or event fields.
- Added one executor-owned `ExecutionScope` shared by top-level and nested precompile dispatch. Begin-block opens it only after dirty-overlay validation; end-block closes it before atomic cleanup and notifies Reth's post-block state hook before state-root calculation.
- Added raw canonical parent point reads and strictly ordered ID-only owner/day/global pages to the Tribute and Nod repositories. `RuntimeBodyReaders` implements the unified parent seam and preserves the ADR-005 distinction between temporary unavailability and deterministic corruption.
- Migrated Tribute and TributeFactory production reads, lists, issue, and burn paths to overlay-first generic operations while preserving their compact business state and product events. Canonical body events are now emitted only by the compressed-entity module.
- Migrated Nod, NodFactory, Lysis, and Cycle to the same typed verified-body lifecycle. Multiple dependent bucket mutations now resolve from the journaled overlay instead of fencing on the finalized Mongo projection.
- Removed arbitrary per-runtime scan caps from Tribute, Nod, and Lysis. Repository page bounds and deterministic EVM gas remain the only ADR-007 work limits.
- Added the four canonical query-index delta surfaces and deterministic parent-page merge, including membership cancellation, strict cursor/order validation, lookahead pagination, overlay-first body resolution, and fatal corruption classification.
- Unified compressed-entity, Nod, and Cycle block hooks under typed `BlockLifecycle` contexts. The executor owns one explicit `ExecutionScope`, opens it before begin-zone execution, closes it before atomic cleanup, and rejects every later compressed read/list/mutation.
- Prepaid body/index touched-list elements, dynamic tails, and both list-length cleanup writes on first touch. Cleanup is checkpointed, deterministic, idempotent over cleared byte tails, and leaves only schema/direct commitments in the temporary v1 stage.
- Planned receipt-visible system gas for the complete block before signing: ordinary phases use intrinsic gas and `CycleTick` receives the remaining block envelope after all other system intrinsic reservations. CE work runs inside that signed window, fails before writes when exhausted, and every receipt remains at or below its transaction gas limit without double-charging user gas.
- Added behavioral verification for all three collection transition matrices, same-value ABA, opaque capability mismatch, canonical event order, exact locator/index/wire vectors, maximum body shrink/delete cleanup, all four list queries, parent corruption, dirty begin, post-cleanup rejection, and golden read/list/cleanup gas coefficients.
- Added deterministic storage/event fault injection that fails each persistent mutation boundary and proves the surrounding checkpoint restores commitments, overlay/index records, touched lists, and logs. No test inspects Rust source text.
- Added a real `OnStateHook` assertion for post-block nonzero-to-zero cleanup changes and full proposer/validator parity over independent parent projections, receipts, gas, balances, state bundle, and trie root. The production payload builder now finalizes CE while the parallel trie hook is attached, detaches it only after cleanup delivery, and then freezes the precomputed root.
- Added criterion harnesses for gas-saturated touch/cleanup and large touched-list merge workloads; the benchmark target compiles in release mode.
- Moved the module to its ADR-owned path, `crates/core/compressed-entities`, while retaining the closed workspace package name and `0xEE0D` system-state address.

### ADR-007 verification boundary

- Formatting, `cargo check --workspace --all-targets`, targeted `-D warnings` clippy, the complete EVM suite, compressed-entity/domain runtime suites (including dense Lysis), and the combined WWD/Lysis/Nod/Gratis plus update-flow E2E tests pass.
- Mongo repository integration cases remain opt-in behind `OUTBE_TEST_MONGODB_URI`; ADR-007 execution parity uses independent in-memory parent projections, while their Mongo adapter contract is covered by the preceding ADR suites.
- Workspace-wide clippy is not a clean project gate because unchanged files still trigger `manual_div_ceil` in `primitives/dispatch.rs` and `needless_borrow` in `offchain-data/tests/projection.rs`; all ADR-007-owned/affected packages excluding those pre-existing findings pass with `-D warnings`.
