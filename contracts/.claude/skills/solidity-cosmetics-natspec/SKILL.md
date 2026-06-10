---
name: solidity-cosmetics-natspec
description: >
  Behavior-preserving Solidity surface hygiene for the contracts/ Forge
  projects. Runs `forge fmt`, drives `forge lint` to zero (and `solhint`
  where a sub-project has it), and applies a judgment pass for NatSpec
  coverage + correctness, comment policy, dead-private-code removal,
  declaration/import/file layout, and interface-completeness (every
  public/external symbol, event, and error on the impl appears in its
  `I<Name>` interface). Strictly pure-surface: never changes storage
  layout, role gating, state machine, cross-chain message format,
  function selectors, or any executable behavior — behavioral-adjacent
  findings are flagged and deferred, never applied. Use whenever Solidity
  needs formatting, NatSpec, comment, or interface-surface cleanup, on one
  contract or the whole set. Companion to solidity-events-errors-audit,
  which owns event/error *sufficiency*.
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

# Solidity Cosmetics + NatSpec Hygiene — `$ARGUMENTS`

Apply behavior-preserving surface hygiene to a Solidity implementation contract and its
interface. Argument is a contract name or `all`. This is the **cosmetics + NatSpec** half
of the convention; the **events/errors sufficiency** half is `solidity-events-errors-audit`
— run this one first when both apply.

A styleguide enforcer: safe to invoke any time, on any contract. It carries no point-in-time
finding list — it derives everything from current code and the toolchain.

## Sub-project model (read first)

`contracts/` holds **one Forge project per directory** (`intent/`, `oft/`, `vault/`,
`smart-account/`, `precompiles/`, `intex/`, …), each with its own `foundry.toml`. Always work
from the **sub-project that owns the target contract** — never from `contracts/` itself.

Two toolchain shapes:

- **Pure-Forge** (the norm: `intent`, `oft`, `vault`, `smart-account`, `precompiles`) — deps via
  **soldeer** (`dependencies/`), commands via `forge`/`make`. Layout: `src/ test/ script/`.
- **Hardhat-hybrid** (currently only `intex`) — has `hardhat.config.ts` + `package.json` +
  `.solhint.json`; sources under `contracts/`, uses **yarn** scripts and TypeChain. The extra
  Hardhat/solhint/TS steps below apply **only** to this shape.

Detect the shape once per target sub-project:
```
test -f <sub>/hardhat.config.ts && echo hardhat-hybrid || echo pure-forge
```

## Target selection

- `<ContractName>` → the impl plus its sibling interface `I<Contract>.sol`, located dynamically
  (works for both `src/` and `contracts/` layouts; exclude deps and test scaffolding):
  ```
  find contracts -name '<Contract>.sol' \
    -not -path '*/interfaces/*' -not -path '*/lib/*' \
    -not -path '*/dependencies/*' -not -path '*/node_modules/*' -not -path '*/vendor/*'
  ```
  The sub-project root is the first path segment under `contracts/`. Find the interface as
  `<dir>/interfaces/I<Contract>.sol` if present.
- `all` → every first-party implementation; discover, never hardcode:
  ```
  find contracts -name '*.sol' \
    -not -path '*/interfaces/*' -not -path '*/lib/*' -not -path '*/dependencies/*' \
    -not -path '*/node_modules/*' -not -path '*/vendor/*' -not -name 'Mock*'
  ```
  `lib/`, `dependencies/`, `node_modules/`, `vendor/`, `interfaces/`, `Mock*`, and test
  scaffolding are out of scope. Group by sub-project and process leaf libraries/types before
  consumers so an interface edit lands first. Review each diff independently.

`internal`/`library`-only files may have no interface — skip the interface-completeness step
for them; check whether an interface exists, never assume.

## THE PURE-SURFACE GATE (read before every edit)

**Provably behavior-preserving at the implementation level**: no deployed-bytecode, storage,
selector, or semantic change. An edit is allowed only if it **cannot** change any of:

- storage layout (slots, ordering, packing, struct field order of stored types);
- role gating / access control;
- state-machine transitions;
- cross-chain message encoding/decoding (any `*MsgCodec` / wire-format library);
- function signatures / **selectors** — do not add, remove, rename, or re-type any `function`
  (a compiler-mandated `override` is allowed; it is selector/bytecode-identical);
- any executable statement, branch, or constant value.

### In scope (apply)
1. `forge fmt` formatting.
2. NatSpec — add missing and correct wrong `@notice`/`@dev`/`@param`/`@return` on
   public/external functions, events, errors, and structs.
3. Comments — delete stale comments referencing removed code, oversized narrative blocks,
   `TODO`/`FIXME`/`HACK`, non-English comments; keep accurate ones.
4. Dead **`private`** code — remove unreachable `private` helpers/constants with zero callers.
   `internal` removal is **not** automatic (see "Dead code is judgment").
5. Layout — import ordering, declaration order (below), file organization. No reordering of
   **stored** struct fields.
6. Interface-completeness for **events and errors** — add the *declaration* of an existing
   impl event/error to `I<Name>` so the interface matches the impl. The low-risk subset.
7. Interface-completeness for **functions** — ABI-additive, handled under the gate in procedure
   step 2 (compile + ABI-diff + consumer propagation). The impl already exposes the function, so
   selector/bytecode are unchanged; only the interface gains an entry.
8. Tooling-config drift that is purely declarative — e.g. aligning a sub-project's
   `.solhint.json` `compiler-version` rule to its `foundry.toml` `solc`. Never change the
   `pragma` or `evm_version` themselves.

### Out of scope — FLAG and DEFER, never apply
Report each in the deferral list with a one-line reason and suggested follow-up; do **not** edit:
- Removing/adding/renaming any **external/public function** or a parameter on one.
- Removing an `internal` symbol without AST-level proof it is not consumed by a derived
  contract or external library user (grep alone is insufficient).
- Extracting helpers / de-duplicating logic → refactor.
- Any new validation/correctness guard (underflow/overflow checks, new `require`/`revert`) →
  behavior.
- Changing `pragma`, `evm_version`, role wiring, or storage → structural.
- **Any event/error _content_ change** — new/renamed/split events, new event params, or
  **removed** errors (including unused ones). All belongs to `solidity-events-errors-audit`;
  here you only *declare* existing events/errors in the interface and *document* them.

## Procedure

Run everything from the target **sub-project** directory (`cd contracts/<sub>`).

### 0. Baseline
```
cd contracts/<sub>
forge build                         # must already be green before you start
git status --short                  # start from a clean tree
```

### 1. Format
```
forge fmt <impl.sol> <interface.sol>
```
Run on the whole sub-project at the end: `forge fmt` (or `make fmt`).

### 2. Interface-completeness sweep (AST-authoritative, never grep)
Single-line `grep` misses multiline declarations (`function wire(` with `external` several
lines down). Enumerate the surface from the compiled ABI:
```
forge inspect <Contract> abi --json  | jq -r '.[] | "\(.type) \(.name // "")"' | sort -u
forge inspect I<Contract> abi --json | jq -r '.[] | "\(.type) \(.name // "")"' | sort -u
```
Diff the two lists.
- Missing **events/errors** → add declarations to the interface (step 6, in-scope).
- Missing **functions** → adding the declaration is ABI-additive. Add it, then:
  1. `forge build` (compiler tells you if the impl now needs `override` / `override(IFoo)`;
     add only that keyword — selector/bytecode unchanged),
  2. refresh ABIs and diff (step 7) — the **only** allowed ABI delta is the added interface
     declaration; nothing on the impl may move,
  3. propagate to any consumers (step 7).
  If any can't be satisfied cleanly, revert the addition and defer it instead.
Only mirror symbols that already exist on the impl — never invent getters.

### 3. NatSpec coverage + correctness (judgment — tools can't do this)
`forge lint`/`solhint` have no NatSpec coverage rule, so this is a read pass over every
public/external function, event, error, and struct:
- **Coverage:** `@notice` present; `@param` per parameter; `@return` per return value; `@dev`
  where a non-obvious invariant or caller obligation exists.
- **Correctness — match the code, not the intent.** Read the body and fix drifted docs.
  Recurring classes: validation claimed but not performed; enum/status/stage docs disagreeing
  with the actual enum; `@dev` describing an operation never called; stale protocol/role names
  or addresses; overstated or wrong bounds.
- `@inheritdoc IXxx` is acceptable when the interface NatSpec is complete and correct; if the
  interface doc is wrong, fix it there.

### 4. Comments + dead code
- Delete stale/oversized/`TODO`/non-English comments; keep accurate rationale.
- **Dead code is judgment:**
  - `private` symbol/constant with zero in-tree callers → remove.
  - `internal` symbol → a contract can be inherited and a library imported outside this repo,
    so grep cannot prove non-use. **Flag and defer** unless you have AST-level proof the
    contract is non-inheritable and the symbol unused.
  ```
  rg -nw '<symbol>' src test script contracts tasks scripts
  ```
  A *public/external* orphan is out of scope (§ Out of scope). Removing an unused `error` is
  also out of scope — event/error content, owned by `solidity-events-errors-audit`.

### 5. Layout
Canonical declaration order: pragma/imports → types → constants → immutables → storage →
events → errors → constructor → external → public → internal → private. Never reorder
**stored** struct fields or storage variables.

### 6. Lint to zero
```
forge lint                                   # pure-Forge: resolve every in-scope finding
# hardhat-hybrid (intex) also has solhint:
yarn lint            # solhint 'contracts/**/*.sol'
yarn lint:fix        # auto-fixable subset
```
Resolve every in-scope **error and warning**. If a finding maps to an out-of-scope behavioral
change, leave it and record it in the deferral list.

### 7. Off-chain / ABI propagation (only if step 2 added a function to an interface)
Refresh exported ABIs and check consumers — never silence failures.

- **Pure-Forge:**
  ```
  make export-abi                       # regenerate abi-export/
  git diff -- abi-export                # confirm only the intended interface delta
  ```
- **Hardhat-hybrid (intex):** declarations also regenerate TypeChain types:
  ```
  yarn compile                          # Hardhat + TypeChain (must succeed)
  yarn cd:extract-abi                   # refresh exported ABI artifacts
  rg -nw '<NewlyExposedSymbol>' tasks scripts test
  git diff -- abi-export                # confirm only the intended interface delta
  ```

NatSpec/comment/layout changes never touch off-chain code.

## Verification gate (must pass before the contract is "done")
```
cd contracts/<sub>
forge fmt --check                                  # forge fmt clean
forge lint                                         # zero in-scope findings
forge build                                        # Foundry compiles
forge test                                         # green
# hardhat-hybrid (intex) additionally:
yarn lint                                          # zero solhint findings (in scope)
yarn compile                                       # Hardhat + TypeChain compile
yarn test                                          # test:hardhat && test:forge — green
yarn analyze                                       # slither + aderyn: no NEW high/critical vs baseline
```
Where a sub-project has `slither`/`aderyn` (`yarn analyze`), treat it as a **regression guard**,
not a fix-to-zero target: compare against the pre-change baseline and allow no *new*
high/critical findings; pre-existing findings are out of scope.

Then assert the **behavior-preserving** invariant: the only ABI delta this skill may produce
is **added interface declarations** (step 2/6). Prove it, don't eyeball it:
```
make export-abi && git diff -- abi-export          # (intex: yarn cd:extract-abi && git diff -- abi-export)
```
Every impl selector, storage slot, opcode, and constant must be unchanged. Any
behavioral diff means the gate is broken — revert and re-scope.

## Output
Report to the conversation (no files written):
1. A short per-contract changelog — what was formatted / documented / removed / exposed.
2. A **deferral list** — every behavioral-adjacent finding you did NOT apply, each with a
   one-line reason and suggested follow-up (especially `internal` dead-code,
   function-signature, and event/error-content items).
3. Confirmation the verification gate passed, including the ABI-diff result.
