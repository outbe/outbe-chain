# T27 — Legacy storage removal and read-path port to CES (tribute/nod; Gem untouched)

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §11.2, §17.1; repo surfaces (see Context)
Depends on: T19, T23, T26, T29, T33 (Variant A runtime adapters must exist before legacy bodies are removed), T36 (the approved port map is this task's decision input — audit-final B-03)
Blocks: — (release gate T25 only)

## Summary

Complete the migration of tribute/nod onto the CES engine (Gem is deferred — its module, views, CLI and
MCP tools stay on legacy EVM storage UNTOUCHED): port every read surface (precompile views,
`outbe-cli`, MCP tools) to the CES read paths and DELETE the legacy per-record EVM body storage. T23 moves
the write path; this task moves the reads and removes the old engine's schemas — after it, CES is the only
storage for record bodies.

## Context

Launch is greenfield (§17.1: no on-chain migration, no legacy body slots to retire), but the CODE cutover has
no owner: T23 explicitly excludes "removal/bypass of legacy per-record EVM body storage" from its first PR.
Surfaces reading storage that CE-active genesis never populates:
- `crates/core/tribute/src/precompile.rs` (`tokenURI`, `ownerOf`, `getTributesByOwner`, `getTributesByDay`,
  `getDayTotals`) and the equivalent Nod view set (Gem views stay legacy-backed and keep working);
- canonical ABIs `contracts/precompiles/src/{ITribute,INod}.sol` (IGem.sol untouched);
- `bin/outbe-cli` tribute/nod commands (`show`, `by_owner`, `by_day`, `day_totals`, `supply`) and
  `bin/outbe-cli/src/abi.rs` mirrors;
- MCP read tools in `mcp/src/tools/view.ts` (`tribute_get`, `tributes_by_owner`, `nod_get`, …; gem tools
  untouched).
§11.2 allows a domain to keep whatever consensus aggregates its rules require (ownership indexes, day
totals) — that boundary must be decided per domain, not left implicit.

## Scope

- APPROVED PORT MAP IS AN INPUT (audit-final B-03 — the decision moved to gate T36 so it PRECEDES T30's
  list-RPC section and T26's implementation): `docs/ces-read-surface-port-map.md` is authored and approved
  under T36 before T30/T26 freeze/implement the list surface; this task IMPLEMENTS the approved
  classification without re-deciding — the (a)/(b)/(c) split of ownerOf/tokenURI/by-owner/by-day/by-WWD/
  totals/supply and the Nod equivalents, plus which aggregates remain consensus state and for what runtime
  reason, are T36 decisions.
- Port map per domain with a hard read-context split: during consensus execution a precompile can read ONLY
  `read_commitment` (leaf/existence, lag-0 overlay) and domain-owned EVM state — proof packages and the
  Mongo projection are off-chain surfaces and MUST NOT back any precompile view. Each current
  `tokenURI/ownerOf/get…` method is therefore classified: (a) keep as a precompile view backed by bounded
  domain-owned consensus state the domain's rules genuinely require (§11.2, e.g. day totals feeding
  emission), re-derived from the new write path; (b) move to the RPC/CLI surface (T18/T19/T26) with an ABI
  breaking-change note; (c) remove. Nothing keeps the legacy schema alive.
- Precompile view sets rewritten onto the ported reads; `.sol` interfaces updated in the same change (repo
  events/ABI conventions).
- `outbe-cli` rewired: point reads via `outbe_getBody` + T18 verifier (client-side proof verification);
  list commands via T26 list RPCs; `abi.rs` mirrors updated; commands whose semantics disappear are removed
  with breaking-change notes (old behavior, new behavior, migration guidance — repo docs contract).
- MCP `view.ts` tools rewired to the same RPC surface.
- Legacy per-record body storage schemas DELETED from the domain modules (`schema.rs`/`state.rs` body
  records, their slots and accessors) — dead layouts do not survive to genesis; module tier/README updated
  per the structure standard.
- README: user-visible CLI/RPC behavior changes documented in the same PR.
- Cutover inventory RE-CHECK (postfix PF-M09 / R1.4 — the inventory itself is generated and approved in
  T36): re-run the T36 selector-to-consumer grep at cutover time; any consumer not present in the
  approved inventory fails this task and the T25 evidence check.

## Out of scope

- New product features on these surfaces; T26's RPC implementation itself; domain business-rule changes.

## Acceptance criteria

1. Port map (T36 artifact) fully IMPLEMENTED: every legacy view/aggregate lands per its approved (a)
   consensus-state precompile view / (b) RPC-CLI move / (c) removal classification with breaking-change
   notes; any remaining domain-owned aggregate justified by a concrete consensus rule (§11.2); no
   precompile view reads proofs or projection data (compile-visible boundary).
2. Legacy body schemas deleted — the types compile out, not just go unread; no code path touches per-record
   legacy body storage; localnet with CE-active genesis serves every ported command/tool correctly; the
   cutover inventory covers ABI exports, scripts, and the full MCP registry (postfix PF-M09).
3. `outbe-cli` point reads verify proofs client-side; list commands paginate via T26; MCP tools return data
   from the projection.
4. Breaking-change documentation for removed/changed commands (old → new mapping).
5. Post-cutover grep-proof (postfix PF-B05, moved from T33 AC1): a repository-grounded grep proves no
   execution-time legacy body read survives the cutover — evidence linked in the T25 ledger.

## Invariants

- No shipped binary carries readers of storage that CE-active genesis never populates.
- Domain-owned aggregates that remain are consensus state by explicit decision, not leftovers.

## Tests

- CLI/MCP integration tests against a CE-active localnet; absence tests for removed selectors.

## Files

- `crates/core/{tribute,nod}/src/{precompile.rs,schema.rs,state.rs}` (gem module untouched)
- `contracts/precompiles/src/{ITribute,INod}.sol`
- `bin/outbe-cli/src/{commands/*,abi.rs}`, `mcp/src/tools/view.ts`, README.md
- `docs/ces-read-surface-port-map.md` (T36-approved INPUT artifact)
