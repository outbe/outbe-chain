---
name: solidity-events-errors-audit
description: >
  Events/errors sufficiency audit for the contracts/ Forge projects —
  judgment-based, interface-surface only. Ensures every privileged
  state rotation emits an event, event payloads are log-reconstructible
  (acted-on key, amounts, old+new value), errors follow
  one-error-one-failure (overloaded errors split, dead errors removed),
  and event/error naming is consistent. Changes are confined to
  event/error declarations and the error type at each existing revert
  site: it never changes WHEN a revert fires, what state changes,
  storage layout, role gating, function selectors, or cross-chain
  message format. EIP-170 size-gated, and every rename is propagated to
  consumers (tests, and TS tasks/scripts where present) in lockstep. Use
  whenever Solidity event/error observability or naming needs review, on
  one contract or the whole set. Companion to solidity-cosmetics-natspec,
  which owns formatting/NatSpec/layout; run that one first.
argument-hint: "<ContractName> | all"
allowed-tools:
  - Read
  - Grep
  - Glob
  - Edit
  - Write
  - Bash(forge *)
  - Bash(make *)
  - Bash(yarn *)
  - Bash(git status *)
  - Bash(git diff *)
  - Bash(rg *)
  - Bash(jq *)
---

# Solidity Events/Errors Sufficiency Audit — `$ARGUMENTS`

Audit and normalize the **observability surface** of a Solidity contract: enough
events to reconstruct privileged state changes from logs alone, and errors that
are precise (one error = one failure) and consistently named. Argument is a
contract name or `all`. This is the **events/errors** half of the convention; the
**cosmetics + NatSpec** half is `solidity-cosmetics-natspec` — run that first.

A styleguide enforcer: safe to invoke any time, on any contract, deriving
everything from the current code (no point-in-time finding list).

## Sub-project model (read first)

`contracts/` holds **one Forge project per directory** (`intent/`, `oft/`, `vault/`,
`smart-account/`, `precompiles/`, `intex/`, …), each with its own `foundry.toml`. Always work
from the **sub-project that owns the target contract** — never from `contracts/` itself.

- **Pure-Forge** (`intent`, `oft`, `vault`, `smart-account`, `precompiles`) — soldeer deps,
  `forge`/`make` commands, layout `src/ test/ script/`.
- **Hardhat-hybrid** (currently only `intex`) — `hardhat.config.ts` + `package.json` +
  `.solhint.json`, sources under `contracts/`, yarn + TypeChain. The Hardhat/yarn/slither/TS
  steps below apply **only** to this shape.

Detect once per target sub-project:
```
test -f <sub>/hardhat.config.ts && echo hardhat-hybrid || echo pure-forge
```

## Target selection
Discover, never hardcode (identical to the companion skill; works for `src/` and `contracts/`):
```
# one contract:
find contracts -name '<Contract>.sol' \
  -not -path '*/interfaces/*' -not -path '*/lib/*' -not -path '*/dependencies/*' \
  -not -path '*/node_modules/*' -not -path '*/vendor/*'
# all first-party impls:
find contracts -name '*.sol' \
  -not -path '*/interfaces/*' -not -path '*/lib/*' -not -path '*/dependencies/*' \
  -not -path '*/node_modules/*' -not -path '*/vendor/*' -not -name 'Mock*'
```
`lib/`, `dependencies/`, `node_modules/`, `vendor/`, `interfaces/`, `Mock*`, and test
scaffolding are out of scope. Events and errors live on the impl and must also be declared on
its interface; edit both.

---

## THE INTERFACE-SURFACE GATE (read before every edit)

This skill may change **only** event/error *declarations* and the *error type
chosen at an existing revert site*. Adding an `emit` to a function that already
performs the state change is observational and allowed. An edit is allowed only
if it **cannot** change any of:

- **WHEN** a revert fires — guard condition, branch, and ordering are fixed; you
  may change `revert OldError()` → `revert NewError()` at the same site, never
  add, remove, or move a check,
- **WHAT** state a function mutates (an added `emit` must not gate or alter a write),
- storage layout, role gating, the state machine,
- function signatures / **selectors**,
- cross-chain message encoding/decoding (any `*MsgCodec` library — its event/error
  surface falls here, but the wire format must not move),
- any computation other than emitting a log or selecting an error type.

Renaming or splitting an error changes its **selector** — the intended, permitted
ABI delta for this skill, but it ripples (see Propagation).

### In scope (apply)
1. **Event on every privileged rotation.** Every `external`/`public` state change
   behind a role/owner gate (setters, `wire`, role grants, `authorize*`, `sweep*`,
   metadata setters) emits an event; if one is missing, add it.
2. **Log-reconstructible payloads.** An event must let an off-chain consumer
   rebuild the change from the log alone: include the acted-on key/id, the amount,
   and the **old + new** value for a rotation. Add missing fields, and `index` the
   key/address fields a consumer filters on.
3. **One error = one failure.** Split an error reused for two distinct failure
   conditions into two precisely-named errors, one per revert site. Carry useful
   context in parameters (e.g. the offending value/label).
4. **Remove dead errors** — declarations with zero `revert` sites (prove with
   `rg`). This is the removal path the companion skill defers here.
5. **Consistent naming.** Events: `Noun + PastParticiple` (`PeerSet`,
   `OwnerUpdated`, `BidsFlushed`). Errors: name the failure
   (`ZeroAddress(string field)`, `Unauthorized(address caller)`,
   `InvalidPayloadLength`). Align outliers.
6. **Interface parity.** Every event/error the impl declares or emits is declared
   on its `I<Name>` interface, and vice-versa (no interface-only ghosts).

### Out of scope — FLAG and DEFER, never apply
- Adding, removing, reordering, or re-conditioning a **check/guard** (changes WHEN
  a revert fires) → behavior; defer to the security/logic ticket.
- Any function-signature, role-wiring, storage, or message-format change.
- Formatting, NatSpec, comments, dead **code** (non-error) removal, layout →
  owned by `solidity-cosmetics-natspec`.
- A new event/error whose payload needs a **new state read** the function lacks in
  scope (extra storage to log it can shift gas/semantics) → flag for the author.

---

## Procedure

Run everything from the target **sub-project** directory (`cd contracts/<sub>`).

### 0. Baseline
```
cd contracts/<sub>
forge build && forge test        # green before you start
git status --short               # clean tree
forge build --sizes              # record current runtime sizes (EIP-170 budget)
```

### 1. Inventory the current surface (AST-authoritative)
```
forge inspect <Contract> abi --json | jq -r '.[] | select(.type=="event" or .type=="error") | "\(.type) \(.name)"' | sort -u
```
Cross-check declarations vs usage on the impl (use the actual source path — `src/` or `contracts/`):
```
rg -n 'emit |revert |error |event ' <path/to/Contract.sol>
```
Build three sets — events emitted, errors reverted, declarations present. The gaps
between them drive the work (missing event, dead error, undeclared symbol).

### 2. Events — sufficiency + reconstructibility
- For each privileged state-mutating function, confirm an event is emitted with
  old+new (or full post-state) and the acted-on key. Add or extend as needed.
- `index` the fields consumers filter by (addresses, ids), max 3 indexed.
- The `emit` goes **after** the state write, never inside a condition that gates it.

### 3. Errors — one-failure + dead removal
- Each distinct failure reverts a distinct, descriptively-named error; split
  overloaded ones site-by-site.
- Remove error declarations with zero revert sites (`rg -nw '<Error>' src test script contracts`).
- Keep error encoding economical — custom-error size counts against EIP-170; prefer few
  parameters over verbose ones on size-critical contracts.

### 4. Naming + interface parity
- Rename outliers to the convention. A rename changes the selector → a real ABI
  change; propagate it (step 6).
- Sync impl ⇄ interface: declare every event/error on the interface; delete
  interface-only ghosts that nothing emits/reverts.

### 5. Size check (EIP-170)
```
forge build --sizes
forge test --match-path 'test/**/*sizes*'        # if size tests exist
```
Adding events/errors grows bytecode. No contract may cross the 24,576-byte limit.
If a change would breach it, economize the encoding or defer the addition and record why.

### 6. Propagate every rename/added-field in lockstep
A changed event/error selector or shape ripples into tests (and off-chain consumers):
```
rg -nw '<OldEventOrErrorName>' src test script tasks scripts config
```
- Update Foundry assertions (`vm.expectRevert`/`vm.expectEmit`) to the new names/shapes.
- **Pure-Forge:** `make export-abi` then `git diff -- abi-export`.
- **Hardhat-hybrid (intex):** also
  ```
  yarn compile          # Hardhat + TypeChain regen (must succeed)
  yarn cd:extract-abi   # refresh exported ABI artifacts
  ```
  and update TS consumers + Hardhat assertions. A stale reference is a failed run.

---

## Verification gate (must pass before the contract is "done")
```
cd contracts/<sub>
forge fmt --check                                  # companion-clean
forge lint                                         # in-scope clean
forge build && forge build --sizes                 # compiles; no contract over EIP-170
forge test                                         # green with updated expectations
# hardhat-hybrid (intex) additionally:
yarn lint                                          # solhint zero (in scope)
yarn compile                                       # Hardhat/TypeChain
yarn test                                          # forge + hardhat green
yarn analyze                                       # slither + aderyn: no NEW high/critical vs baseline
rg -nw '<AnyOldName>' src test script tasks scripts config   # zero stale references
```
Then prove the ABI delta is **only** event/error declarations — no function
selector, storage slot, or message-format change:
```
make export-abi && git diff -- abi-export          # (intex: yarn cd:extract-abi && git diff -- abi-export)
```
Every function selector and storage slot must be unchanged; revert *conditions*
and state writes identical. Any other diff means the gate is broken — revert and
re-scope.

## Output
Report to the conversation (no files written):
1. A per-contract changelog — events added/extended, errors split/removed/renamed,
   interface parity fixes.
2. A **deferral list** — every observability gap left open because it would need a
   new guard, new state read, or behavior change, each with a one-line reason and
   suggested follow-up.
3. Confirmation the verification gate passed: size check, stale-reference sweep,
   and ABI-diff result.
