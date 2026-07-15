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
