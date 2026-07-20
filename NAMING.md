# Repository Naming Canon

Status: normative for new and changed names

Scope: the whole `outbe-chain` repository

Audience: maintainers, reviewers, and coding agents

Last verified against the repository: 2026-07-20

## 1. Authority and evidence

### 1.1 Purpose and authority

Names in this repository are part of the protocol design. A useful name lets a
reader predict the domain concept, programming role, units, lifecycle state,
direction, and side effects without opening a distant implementation.

This file defines the naming rules for the repository. It is not a bulk rename
plan and it does not declare every existing name correct. Apply it to new code
and to names touched by a change. Existing public, persisted, serialized, or
standard-owned names require an explicit compatibility decision before they are
changed.

`NAMING.md` is the repository-wide naming authority. Module audit reports may
identify migration work, but they do not create vocabulary merely by proposing
it.

The repository has two principal language surfaces:

- Rust under `crates/**` and `bin/**`;
- Solidity under `contracts/**`.

The universal semantic rules apply to both. Language-specific casing and API
grammar are defined separately below.

Normative words have their usual meaning:

- **MUST**: required for new or changed names;
- **SHOULD**: expected unless a documented local or external convention wins;
- **MAY**: allowed when it remains semantically truthful.

### 1.2 Evidence order

When sources disagree, use this order:

1. enforced external contract or domain specification;
2. tested behavior and data shape;
3. implementation and meaningful call sites;
4. dominant vocabulary in the same bounded context;
5. current comments and prose documentation;
6. generic language or framework convention.

Call out a conflict instead of silently choosing a lower-ranked source.
The root [README](README.md) describes the intended external system, while code
and tests show what is currently enforced. [CLAUDE.md](CLAUDE.md) defines the
repository's module structure and safety rules. Solidity standards and imported
interfaces outrank project style for names they own.

### 1.3 Semantic evidence and lexical evidence are separate gates

Before creating or recommending a non-trivial name, an agent MUST establish:

1. **Verified meaning**: what the symbol represents, does, returns, and affects.
2. **Lexical provenance**: where the proposed words are already established.
3. **Mold**: the grammar and casing for that symbol kind in that language.

Proving the meaning does not prove the words. A representation, scale, or
implementation detail does not license importing a familiar industry term when
that term is absent from the governing specification and bounded context.

If a useful word has no lexical provenance, it MUST NOT be presented as
canonical. Mark it explicitly as **new vocabulary** with a rationale, or place
it under **uncertain candidates** until the domain owner accepts it.

### 1.4 Bounded contexts own words

The same English word may have a different contract in another bounded context.
Do not normalize names across contexts until their meanings are proven equal.

| Path                         | Vocabulary owner                                                                        |
| ---------------------------- | --------------------------------------------------------------------------------------- |
| `crates/blockchain/**`       | execution, consensus integration, node, RPC, txpool, storage infrastructure             |
| `crates/system/**`           | system protocol modules and block-level protocol state                                  |
| `crates/core/**`             | business modules, precompiles, and cross-module workflows                               |
| `crates/testing/**`          | reusable test infrastructure; it consumes, but does not redefine, production vocabulary |
| `contracts/precompiles/**`   | canonical external ABI for Rust runtime precompiles                                     |
| `contracts/crosschain/**`    | ERC-7786 gateways, bridge routing, and interoperable addresses                          |
| `contracts/intent/**`        | ERC-7683 orders, solvers, settlement, collateral, and routing                           |
| `contracts/intex/**`         | Intex auction and cross-chain Intex flow                                                |
| `contracts/smart-account/**` | ERC-4337/7579 smart accounts, permissions, hooks, policies, and bundles                 |
| `contracts/tokens/**`        | token and token-bridge contracts                                                        |

Tests, adapters, and end-to-end crates MUST reuse the production owner's term.
They MUST NOT introduce a synonym merely because it is convenient in a test or
transport layer.

### 1.5 Excluded sources

Generated and vendored code does not establish project vocabulary. In
particular, do not derive naming rules from:

- `contracts/**/out/**`;
- `contracts/**/abi-export/**`;
- `contracts/**/vendor/**` and `contracts/intex/src/vendor/**`;
- generated Rust bindings;
- fixtures, snapshots, migration history, or copied third-party sources.

Change the source interface or generator input instead of editing a generated
name.

## 2. External language baselines

External language guidance supplies the default mold after meaning and lexical
provenance have been established. It does not override an enforced external
contract, standard-owned name, tested behavior, or an explicit rule in this
file.

### 2.1 Rust API Guidelines

The official [Rust API Guidelines: Naming](https://rust-lang.github.io/api-guidelines/naming.html)
are the baseline for project-owned Rust APIs. In particular:

- type-level constructs use `UpperCamelCase`, value-level constructs use
  `snake_case`, and constants/statics use `SCREAMING_SNAKE_CASE`;
- acronyms count as words, so project-owned types use `Dkg`, `Rpc`, and `Uuid`
  rather than `DKG`, `RPC`, and `UUID` inside `UpperCamelCase` identifiers;
- `as_*` is a cheap borrowed view, `to_*` performs work or produces an owned
  value without consuming a non-`Copy` receiver, and `into_*` consumes the
  receiver;
- simple getters omit `get_`: use `value()` and `value_mut()` rather than
  `get_value()` and `get_value_mut()`;
- related names keep a consistent word order, including established molds such
  as verb-object-error.

Section 5 defines the repository-specific Rust application and the narrow cases
where an established domain lookup or compatibility boundary retains `get_*`.

### 2.2 Solidity Style Guide

The official [Solidity Style Guide: Naming Conventions](https://docs.soliditylang.org/en/latest/style-guide.html#naming-conventions)
is the baseline for project-owned Solidity names. It uses CapWords for contracts,
libraries, structs, events, and enums; mixedCase for functions, arguments,
modifiers, locals, and state variables; and upper case with underscores for
constants. It also recommends a leading underscore for non-external functions
and state variables.

The Solidity guide explicitly describes itself as evolving guidance rather than
an absolute rule and says that project-specific, module-local consistency takes
precedence when conventions conflict. Sections 3 and 6 therefore define the
project's deliberate choices; standard-owned ABI names remain unchanged.

### 2.3 External specifications and APIs

Names owned by ERC/EIP specifications, Ethereum, OpenZeppelin, Reth, Commonware,
or another integrated protocol MUST retain the owner's concepts and spelling at
that boundary. A wrapper MAY adapt only the language mold unless it intentionally
exposes a different project-level concept.

## 3. Project-specific overrides

These rules refine the external baselines for this repository:

- Semantic and lexical evidence in section 1 is mandatory. A language style
  guide cannot make an unsupported domain word canonical.
- Existing public, persisted, serialized, ABI, event, wire, CLI, environment,
  and standard-owned names are compatibility-controlled. Cosmetic consistency
  alone is not authority to rename them.
- Rust simple field accessors follow the official noun/`_mut` convention.
  `get_*` is reserved for an established domain lookup, collection-style lookup,
  or compatibility-controlled API where absence semantics are part of the
  return contract; it is not the default spelling for a getter.
- Established Cargo/package compounds such as `validatorset`, `teeregistry`,
  and `vaultprovider` remain local spellings. Do not mechanically re-segment
  them from ordinary English.
- Solidity names owned by an external standard preserve the standard's casing,
  initialisms, and grammar even when the project-owned mold differs.
- A bounded context owns its vocabulary. Cross-module consistency is desirable
  only after the concepts are proven equivalent.
- When an existing module is internally consistent but differs from a general
  baseline, preserve it unless the change deliberately migrates the entire safe
  surface and section 8 permits the migration.

Project-specific consistency never means copying an accidental typo or stale
historical label into new code. The exception MUST be supported by an external
contract, compatibility constraint, dominant bounded-context vocabulary, or an
explicit rule in this file.

## 4. Shared domain vocabulary

### 4.1 Semantic roles and suffixes

Use these suffixes only for the stated role. A suffix is a semantic promise, not
decoration.

| Form                     | Meaning                                                                  | Repository examples                                   |
| ------------------------ | ------------------------------------------------------------------------ | ----------------------------------------------------- |
| `*Id`                    | stable identity of a domain entity or protocol operation                 | `DkgCeremonyId`, `TriggerId`                          |
| `*Key`                   | lookup, storage, or cryptographic key; not automatically entity identity | `Key`, `consensus_pubkey`                             |
| `*Handle`                | opaque reference or capability used to perform later work                | `StorageReaderHandle`, `StorageHandle`                |
| `*Config`                | operator- or construction-time configuration                             | `ProjectionConfig`, `MongoStorageConfig`              |
| `*Params`                | inputs to one command or operation                                       | `BootstrapParams`                                     |
| `*Request` / `*Response` | a transport or request/response boundary                                 | `EnclaveRequest`, `EnclaveResponse`                   |
| `*Context`               | ambient data and capabilities required by an operation                   | `BlockContext`, `BlockRuntimeContext`                 |
| `*Record`                | persisted record with schema meaning                                     | `ProposalRecord`, `ScheduledUpdateRecord`             |
| `*State`                 | mutable aggregate state or a state-machine value                         | `ProjectionState`                                     |
| `*Status`                | compact classification of an entity's current condition                  | `ProposalStatus`, `ScheduledUpdateStatus`             |
| `*Stage`                 | ordered phase inside a workflow                                          | `AuctionStage`                                        |
| `*Kind` / `*Type`        | stable classification, not lifecycle progress                            | use the bounded context's established noun            |
| `*Entry`                 | one item in an indexed, scanned, or log-like result                      | `ScanEntry`                                           |
| `*Page`                  | bounded page plus continuation information                               | `ScanPage`                                            |
| `*Result`                | computed return value with no claim of terminal lifecycle meaning        | a domain-specific calculation result                  |
| `*Outcome`               | explicit branch or domain outcome of an operation                        | `ProjectionOutcome`, `CeremonyOutcome`                |
| `*Artifact`              | material produced for another protocol stage or subsystem                | `ExecutionSummaryArtifact`, `ConsensusHeaderArtifact` |
| `*Snapshot`              | point-in-time view intended to be read as a unit                         | use only when point-in-time semantics are real        |
| `*Proof`                 | evidence intended for a verification procedure                           | `FinalizationProof`                                   |
| `*Evidence`              | material supporting a protocol claim or fault decision                   | `InvalidVrfProofEvidence`                             |
| `*Commitment`            | an actual cryptographic commitment                                       | use only when commitment semantics are verified       |
| `*Reader` / `*Writer`    | read-only or write-capable port                                          | `StorageReader`, `StorageWriter`                      |
| `*Provider`              | supplies a capability or value behind a defined port                     | use only when the supplied capability is named        |
| `*Registry`              | authoritative association or membership registry                         | `TeeRegistry`                                         |
| `*Client`                | client for a named external or process boundary                          | `UpstreamRpcClient`                                   |
| `*Lifecycle`             | owner of block lifecycle entrypoints                                     | `CycleLifecycle`                                      |
| `*LifecycleContext`      | typed dependencies for one lifecycle owner                               | `CycleLifecycleContext`                               |
| `*Error`                 | structured failure family                                                | `ProjectionError`, `ZkProofError`                     |

Rules:

- Do not use `Data` for a generic persisted record, mutable state, request, or
  response. Keep `Data` only when an external specification owns the name or the
  bounded context defines a genuine payload shape with that term.
- Do not use `Info`, `Details`, `Object`, `Item`, `Manager`, `Helper`, `Util`,
  `Service`, or `Processor` when a more specific established role exists.
- Do not use `State`, `Status`, and `Stage` as synonyms in one lifecycle.
- Do not use `Id`, `Key`, `Handle`, `Hash`, `Commitment`, `Proof`, or `Signature`
  interchangeably. Their security and identity semantics differ.
- An adjective such as `Pending`, `Prepared`, `Ready`, `Loaded`, `Stored`, or
  `Finalized` MUST correspond to an observable lifecycle or capability
  distinction. It MUST NOT be decorative.
- Collection names are plural. Scalar names are singular.

### 4.2 Values, units, time, and direction

#### 4.2.1 Numeric values

Prefer a strong domain type when the same primitive can represent different
concepts. At every public, persisted, ABI, or wire boundary, a numeric name or
its type/documentation MUST make its meaning and representation recoverable.

- `*_count` is a number of objects.
- `*_index` is a zero- or one-based position whose convention is documented.
- `*_amount` is a quantity of an asset or value.
- `*_amount_minor` is allowed only when the bounded context defines the value as
  minimal currency units.
- `*_rate` denotes a domain rate. Its scale and rounding rules MUST be defined by
  a strong type, governing interface/specification, or adjacent boundary
  documentation.
- `*_price` MUST identify the priced asset and quote unit when they are not
  unambiguous in the bounded context.
- `*_shares` and `*_amount` are distinct when shares represent a claim rather
  than the underlying asset.

Do not derive a unit label from the storage width or scale alone. Existing
representation constants such as `SCALE_1E18` state an encoding fact; they do not
by themselves create a new domain noun.

#### 4.2.2 Time and chain position

Use the name that matches the actual clock or ordering domain:

| Form                              | Meaning                                                        |
| --------------------------------- | -------------------------------------------------------------- |
| `*_at`                            | timestamp at which an event occurred or is scheduled           |
| `*_timestamp`                     | raw timestamp when the explicit representation matters         |
| `*_seconds`, `*_millis`, `*_days` | duration in the stated unit                                    |
| `*_block_number` / `*_height`     | the same block coordinate; choose the word by owner and role   |
| `epoch`                           | protocol epoch                                                 |
| `view`                            | Commonware Simplex view; do not casually replace with `round`  |
| `worldwide_day` / `WorldwideDay`  | the established domain day identifier, not a generic timestamp |

`block_number` and `height` are not different units in Outbe. The distinction
is lexical ownership, not value semantics:

- use `block_number` for a block identity obtained from or exposed through the
  Ethereum/Reth/EVM model, including headers, runtime block context, ABI/wire
  fields, and persisted references such as `parent_block_number` or
  `finalized_block_number`;
- use `height` when an external consensus API owns that word, notably
  Commonware's `Height`, or when an established consensus/progress concept names
  a position such as `consensus_tip_height`, `freeze_height`, or
  `planned_activation_height`;
- when two subsystems expose the same coordinate, qualify the owner or role,
  such as `consensus_tip_height` and `reth_head_height`; do not pretend that one
  value is a height and the other is a different kind of block number.

This equivalence is enforced in live code:
`ConsensusBlock::height()` wraps `self.number()`, and compressed-entity proof
validation requires `marker.height == header.block_number`. Therefore, do not
introduce both forms in one bounded context as competing names for the same
role. Preserve `height` where the external or established vocabulary owns it;
otherwise prefer `block_number` for a specific Outbe block identity.

Do not use `date`, `time`, `period`, or `deadline` without enough context to
recover the representation and comparison rule.

#### 4.2.3 Direction, ownership, and actors

Use established role words rather than positional placeholders:

- `source` / `destination` for transfer direction;
- `origin` / `target` only where the bounded context or external standard uses
  those roles;
- `previous` / `current` / `next` for ordered versions or states;
- `sender` / `recipient` for message or value transfer;
- `owner`, `admin`, `authority`, `validator`, `relayer`, `solver`, `allocator`,
  and `bundler` only for their actual authorization or protocol role.

Names such as `from`, `to`, `caller`, or `account` are acceptable when their
meaning is immediate and standard at the local boundary. Do not call an address
an `id`, or a public key an `address`, merely because both are byte strings.

#### 4.2.4 Booleans and optional values

- Boolean fields and predicates SHOULD use `is_*`, `has_*`, `can_*`,
  `should_*`, or a locally established predicate such as `supports_*`.
- Positive names are preferred over double negation.
- A predicate-shaped name MUST return a boolean.
- Optionality belongs in the type. Add `optional` to a name only when it denotes
  a domain concept, not merely `Option<T>` or a nullable ABI field.

### 4.3 Actions and lifecycle grammar

Commands are imperative and name the primary observable effect.

| Verb                                                 | Contract                                                          |
| ---------------------------------------------------- | ----------------------------------------------------------------- |
| `register`                                           | create an authoritative association or membership entry           |
| `activate` / `deactivate`                            | change eligibility or operational status                          |
| `issue`                                              | create and grant a business-domain instrument or entitlement      |
| `mint` / `burn`                                      | change ledger or token supply                                     |
| `record`                                             | persist a fact that has already been established                  |
| `submit`                                             | hand a candidate/request/proof to an authority for later decision |
| `request`                                            | initiate a workflow whose success is not yet established          |
| `start` / `advance` / `complete` / `fail` / `cancel` | explicit workflow transitions                                     |
| `settle`                                             | discharge an obligation or finalize an economic exchange          |
| `claim`                                              | exercise an established entitlement                               |
| `lock` / `unlock` / `slash`                          | collateral or custody transitions with those exact effects        |
| `encode` / `decode`                                  | convert to or from a defined representation                       |
| `parse`                                              | interpret syntax into a value                                     |
| `validate`                                           | check structural or domain constraints                            |
| `verify`                                             | check a proof, signature, claim, or other evidence                |
| `add` / `remove`                                     | simple collection membership only                                 |

Do not hide a multi-module business workflow behind a narrower ledger verb. Do
not use `process`, `handle`, `execute`, `update`, or `manage` when a more precise
established verb describes the effect. These generic verbs remain valid when the
bounded context genuinely defines a multi-branch operation with that name and
its input/output contract makes the branches explicit.

Events are facts, not commands. Prefer an exact past-tense outcome such as
`ValidatorRegistered`, `AuctionCleared`, or `MessageExecuted`. Avoid a generic
`Processed` event when only one branch actually occurred.

### 4.4 Project spellings and protocol distinctions

#### 4.4.1 Proper and protocol names

Use established project spellings consistently:

- `Outbe`, `COEN`, `WCOEN`, `USDT`, `USDT0`;
- `Gratis`, `Promis`, `Tribute`, `Nod`, `Gem`;
- `Intex`, `Desis`, `Metadosis`, `Credis`, `Anadosis`, `Fidelity`;
- `WorldwideDay`, `ValidatorSet`, `SlashIndicator`, `TeeRegistry` in Rust type
  casing; their snake-case forms in modules and fields;
- `Reth` and `Commonware Simplex` for the integrated systems.

Do not translate, pluralize, or respell a proper domain name to make it look like
ordinary English. If a module's documentation defines a more precise expansion
or role, that bounded-context definition wins.

#### 4.4.2 Protocol distinctions that names must preserve

These terms are related but not interchangeable:

- `validator set` is the broader registered/protocol set; `active consensus set`,
  `committee`, `pending validators`, `admitted non-consensus validators`, and
  `reshare target set` name distinct selections or lifecycle roles;
- `epoch` and `view` are separate consensus coordinates;
- `canonical`, `finalized`, `executed`, and `projected` describe different
  guarantees or processing stages;
- `DKG ceremony`, `reshare`, `dealer log`, and `boundary artifact` are distinct
  parts of validator-key lifecycle;
- a `system transaction` is a protocol-inserted execution input, not a user
  transaction or an arbitrary internal call;
- a `precompile` is an ABI execution boundary; a `lifecycle` component owns
  block hooks; a `runtime` component owns business orchestration;
- `offchain storage` is a storage capability, `offchain data` is a system
  protocol module, a `compressed entity` is a domain representation, and a
  `projection` is the process/state that persists finalized results;
- a block `artifact`, encoded `body`, storage `record`, and emitted `event` are
  different representations with different compatibility rules.

Do not replace one of these with a nearby synonym without proving equivalence in
the owning implementation and specification.

#### 4.4.3 Abbreviations

An abbreviation is allowed on a public boundary only when at least one of these
is true:

1. an external standard owns it;
2. it is dominant throughout the bounded context;
3. the full phrase is less recognizable to the intended reader.

Established families include protocol/technical forms such as `ABI`, `API`,
`RPC`, `EVM`, `EOA`, `BLS`, `DKG`, `VRF`, `P2P`, `TEE`, `ZK`, `ERC`, and `EIP`,
plus domain abbreviations explicitly defined by their module documentation.
Their Rust casing still follows section 5.1.

Unfamiliar fragments MUST be expanded on public, persisted, ABI, storage, and
wire boundaries. A short local name remains acceptable in a tiny obvious scope
(`i`, `tx`, `id`) when expansion would add no retrieval value.

Do not invent an abbreviation, unit label, encoding name, or financial term from
the primitive representation alone.

## 5. Rust rules (`crates/**` and `bin/**`)

### 5.1 Casing and acronyms

| Symbol                      | Form                                                 | Examples                                                  |
| --------------------------- | ---------------------------------------------------- | --------------------------------------------------------- |
| Cargo package               | `outbe-<lowercase crate name>`                       | `outbe-offchain-storage`, `outbe-validatorset`            |
| crate directory             | lowercase; preserve established domain compounds     | `compressed-entities`, `offchain-storage`, `validatorset` |
| module/file                 | `snake_case`                                         | `offchain_data.rs`, `sol_ext.rs`                          |
| type/trait/enum/variant     | `UpperCamelCase`                                     | `DkgBoundaryArtifact`, `ScheduledUpdateStatus`            |
| function/method/local/field | `snake_case`                                         | `get_vrf_seed`, `consensus_pubkey`                        |
| constant/static             | `SCREAMING_SNAKE_CASE`                               | `SCALE_1E18`, `MAX_NAMESPACE_BYTES`                       |
| feature                     | lowercase kebab-case unless Cargo requires otherwise | follow the owning crate's existing feature family         |

The `outbe-` prefix is stable, but the suffix is not mechanically re-segmented:
infrastructure packages use names such as `offchain-storage`, while established
domain modules use compounds such as `validatorset`, `teeregistry`, and
`vaultprovider`. Reuse the bounded context's spelling instead of inventing a
second package form.

Rust acronyms use word casing inside identifiers: `Rpc`, `Evm`, `Dkg`, `Vrf`,
`P2p`, `Tee`, and `Zk`. Preserve canonical external type spellings such as
`U256` and `B256`. In snake case, use `rpc`, `evm`, `dkg`, `vrf`, `p2p`, `tee`,
and `zk`.

Do not create mixed forms such as `RPCClient`, `EVMConfig`, or `DKGOutcome` for
project-owned Rust types when `RpcClient`, `EvmConfig`, and `DkgOutcome` fit the
established mold.

### 5.2 Module and file names

Runtime modules follow the file responsibilities defined in
[CLAUDE.md](CLAUDE.md):

| File                               | Owns                                                        |
| ---------------------------------- | ----------------------------------------------------------- |
| `schema.rs`                        | storage schema, persisted records, and storage status types |
| `state.rs`                         | local CRUD, indexes, and state transitions                  |
| `migration.rs`                     | schema evolution                                            |
| `runtime.rs`                       | business logic and cross-module orchestration               |
| `constants.rs`                     | module-local constants                                      |
| `precompile.rs`                    | inbound ABI decode, dispatch, and encode only               |
| `sol_ext.rs`                       | outbound Solidity ABI declarations                          |
| `rpc.rs`                           | `outbe_*` RPC adapters                                      |
| `lifecycle.rs`                     | thin begin/end-block entrypoints                            |
| `<name>_hook.rs`, `<name>_sink.rs` | specialized entrypoints named for the actual hook/sink      |
| `api.rs`                           | stable cross-module Rust API                                |
| `errors.rs`                        | module-specific structured errors                           |
| `genesis.rs`                       | genesis and initialization shapes                           |
| `lib.rs`, `mod.rs`                 | wiring and minimal intentional re-exports                   |

For new runtime modules, `contract.rs`, `logic.rs`, `storage.rs`, and
`orchestrator.rs` are legacy file names. Do not introduce `events.rs`: canonical
external events live in `contracts/precompiles/src/I<Module>.sol` and are
imported through `sol!` bindings.

A file or module MUST be named for its primary responsibility. If no concise
truthful name exists because it contains unrelated responsibilities, investigate
the boundary instead of creating an essay-length filename.

### 5.3 Types and traits

- Name concrete domain types with a noun: `ProtocolVersion`, `Namespace`,
  `StoredValue`.
- Name capability traits by the capability: `StorageReader`, `StorageWriter`.
- Use `*Handle` only for an opaque capability/reference, not as a synonym for
  identifier or wrapper.
- Use `*Contract` only for an actual EVM/storage facade when that bounded context
  already uses the suffix. Do not use it for an arbitrary service or workflow.
- Use `*Factory` only when the bounded context already defines the component as
  a factory or it actually creates/deploys/assembles instances. Existing
  project-owned `*Factory` module names are compatibility-controlled; they do
  not make the suffix a default for new orchestrators.
- Avoid generic `Manager`, `Helper`, `Util`, and `Service` traits or structs.
  Name the owned capability instead.
- Use newtypes when two public values have the same primitive representation but
  different domain meaning or validation rules.

### 5.4 Conversions and accessors

Use the ownership and cost promise from the Rust API Guidelines:

| Prefix/shape            | Contract                                                       |
| ----------------------- | -------------------------------------------------------------- |
| `as_*(&self)`           | cheap borrowed view into the same value or representation      |
| `to_*(&self)`           | performs work and/or produces an owned value without consuming |
| `into_*(self)`          | consumes the receiver and returns an owned value               |
| `<noun>(&self)`         | simple shared accessor                                         |
| `<noun>_mut(&mut self)` | simple mutable accessor                                        |
| `into_inner(self)`      | consumes a single-value wrapper and returns its wrapped value  |

Do not call a non-trivial conversion `as_*`, and do not call a borrowed accessor
`into_*`. A simple getter such as `height()` or `config()` omits `get_`.

### 5.5 Rust queries

For new or changed Rust APIs, use this repository grammar unless a standard
trait fixes the method name. The return type and error documentation remain part
of the contract; the verb does not replace them.

| Prefix                                 | Meaning                                                                                 |
| -------------------------------------- | --------------------------------------------------------------------------------------- |
| `get_*`                                | established domain/collection lookup; absence semantics are explicit in the return type |
| `require_*`                            | enforce a named precondition or extract a value whose absence is an error               |
| `load_*`                               | acquire a file/key/state value or return an entity with required context/capability     |
| `read_*`                               | direct storage, provider, bytes, metadata, or schema-level read                         |
| `list_*`                               | returns a collection or page                                                            |
| `find_*`                               | searches by criteria and may return zero, one, or many results as the type states       |
| `is_*`, `has_*`, `can_*`, `supports_*` | boolean predicate                                                                       |

`get_*` is not a synonym for a simple field accessor. Use it only when lookup is
the established operation or compatibility controls the existing API. Do not
mechanically rename compatibility-controlled APIs to satisfy this table; apply
it when designing a new boundary or deliberately migrating an old one.

### 5.6 Rust storage and state

- Persisted schema types SHOULD end in `Record` when they represent one stored
  domain record.
- A schema field holding many records is plural; a single record is singular.
- Storage keys state the indexed concept; do not call every key `<Entity>Id`.
- `#[storage_schema]`, `#[storage_record]`, `#[key]`, and
  `#[attribute(order = N)]` names participate in persisted compatibility. A
  rename MUST preserve or migrate the actual layout and access contract.
- Local CRUD stays in `state.rs`; a neighboring module is called through its
  public `api.rs`/re-export, so the caller reuses the neighbor's vocabulary.
- Consensus-visible collections use deterministic concepts and ordering. A
  `BTreeMap` is still named for its domain contents, not for its container type.

Do not encode Rust container or primitive types into field names (`vec_`,
`u256_`, `map_`). Encode the domain role, cardinality, unit, or ordering rule
instead.

### 5.7 Errors

- Public error families use `<Domain>Error` or `<Operation>Error`:
  `ProjectionError`, `ProtocolVersionParseError`, `P2pAddressError`.
- Variants state the cause, not the place where it was noticed.
- Avoid `Failed`, `Unknown`, `Invalid`, or `Internal` without the missing or
  invalid concept.
- Preserve the distinction between parse, validation, authorization, storage,
  transport, and verification failures.
- A local `Result<T>` alias is acceptable when its error type is unambiguous in
  the module. Do not create domain structs named only `Result` or `Error`.

### 5.8 RPC, serialization, CLI, and environment boundaries

- Rust RPC traits and methods use Rust casing; the existing JSON-RPC namespace
  is `outbe` and external methods use their declared lower-camel spelling.
- Serialized field names follow the boundary's declared convention, commonly
  `camelCase` for `outbe_*` RPC DTOs. Do not infer a serialized rename from a
  Rust field rename; use an explicit compatibility attribute when needed.
- CLI commands and flags use kebab-case.
- Environment variables use `OUTBE_`-prefixed `SCREAMING_SNAKE_CASE` when they
  are project-owned. Preserve external tool variables exactly.
- Transport DTO suffixes such as `Info`, `Request`, and `Response` are allowed
  only at a real transport boundary. Do not move them into the domain model.

### 5.9 Rust tests

Rust test names SHOULD read as behavior, condition, and outcome in snake case:

```text
<behavior>_<condition>_<outcome>
```

Use the shortest form that remains specific. Test modules follow the repository
tier rules: `tests.rs` first, then focused `tests/common.rs`, `tests/state.rs`,
`tests/lifecycle.rs`, and `tests/e2e.rs` as the module grows. Test helpers and
fixtures reuse production terms and do not establish aliases.

## 6. Solidity rules (`contracts/**`)

### 6.1 Casing and source layout

| Symbol                            | Form                           | Examples                                   |
| --------------------------------- | ------------------------------ | ------------------------------------------ |
| contract/library/struct/enum      | `PascalCase`                   | `ERC7786TokenBridge`, `AuctionSchedule`    |
| interface                         | `I` + canonical type name      | `IValidatorSet`, `IERC7786Recipient`       |
| function/modifier/parameter/local | `lowerCamelCase`               | `setRemoteBridge`, `destinationDomain`     |
| event                             | `PascalCase` fact              | `RemoteBridgeRegistered`, `AuctionCleared` |
| custom error                      | `PascalCase` cause             | `InvalidRecipient`, `UnauthorizedBridge`   |
| constant                          | `SCREAMING_SNAKE_CASE`         | `GAS_LIMIT_ATTR`, `DAILY_LIMIT`            |
| private/internal function         | leading `_` + `lowerCamelCase` | `_fillOrder`, `_onClaimed`                 |
| private/internal state            | leading `_` + `lowerCamelCase` | `_remotes`                                 |
| enum member                       | `PascalCase`                   | `LockUnlock`, `BurnMint`, `Cleared`        |

Project source files contain one primary contract/interface/library and use the
same PascalCase name. Foundry tests use `.t.sol`; scripts use `.s.sol`.

Constructor and initializer parameters use descriptive lowerCamelCase names. A
trailing underscore such as `owner_` MAY resolve a direct collision with a state
name; use it consistently within that contract. Do not alternate `owner_` and
`_owner` without a framework or inheritance reason.

Constants are `SCREAMING_SNAKE_CASE`. Immutables are values, not constants for
naming purposes: new project-owned immutables use `lowerCamelCase`, with a
leading underscore when private. Preserve a local legacy convention when a
cosmetic change would add churn without improving meaning.

### 6.2 Contracts, interfaces, libraries, and standards

- An interface is named `I<CapabilityOrContract>` and describes a real external
  surface.
- A base contract uses the established `Base` suffix only when it provides an
  inheritance implementation seam, as in `OriginSettlerBase`.
- An adapter names both the capability and its role where needed:
  `EscrowAdapter`, gateway-specific adapters.
- A router owns routing. A bridge owns transfer/custody behavior. A settler owns
  settlement. Do not use these nouns interchangeably.
- A library is named for the representation or operation family it owns, not
  `Utils` or `Helpers`.
- Preserve standard-owned names and spellings exactly: ERC/EIP interface names,
  callbacks, events, structs, errors, and functions are not project vocabulary
  to normalize.

Standard compounds and project compounds may intentionally differ. For example,
an external standard may own `CrossChain` while an established project surface
uses `Crosschain`. Preserve the standard name at its boundary and use one
spelling consistently inside each project-owned bounded context.

### 6.3 Solidity functions and getters

External mutating functions are imperative and name the business effect:
`submitQuote`, `claimOrder`, `setRemoteBridge`, `crosschainMint`.

Use this view-function grammar:

| Form                                      | Meaning                                                                           |
| ----------------------------------------- | --------------------------------------------------------------------------------- |
| noun/public variable getter               | direct exposed state value                                                        |
| `get<Noun>`                               | computed or structured lookup; absence/revert semantics are documented in NatSpec |
| `is<Noun>`, `has<Noun>`, `supports<Noun>` | boolean query                                                                     |
| `<noun>Count`                             | collection cardinality                                                            |
| `<noun>At(index)`                         | indexed enumeration                                                               |
| `quote<Action>`                           | fee or amount quotation for a later action                                        |

The Rust `get_*`/`require_*` distinction does not apply mechanically to Solidity
ABI names. Existing interfaces and standards control Solidity getter grammar.

Use `set<Noun>` only for replacement/configuration. Use `add`, `register`,
`remove`, `revoke`, `pause`, and `unpause` when those exact state transitions
occur. Internal hooks use `_on<PastParticiple>` when they react to an established
transition, such as `_onClaimed` and `_onFilled`.

### 6.4 Structs, storage, parameters, and mappings

- Structs are nouns that describe the represented payload or record:
  `AuctionSchedule`, `WithdrawalLimitState`, `SubmittedBidData` when that name is
  owned by the interface.
- Parameter names state the role, not merely the Solidity type. Prefer
  `destinationDomain`, `orderId`, and `recipient` over `domain_`, `value`, and
  `data` when the bounded context has the precise word.
- Name mapping keys and values in the declaration when it improves ABI/source
  readability: `mapping(uint32 domain => bytes recipient)`.
- Mappings and arrays are plural unless the collection itself is a singular
  domain object.
- Use `calldata`, `memory`, and `storage` for data location; do not repeat the
  location in the variable name.
- Distinguish `payload`, `body`, `message`, `extraData`, `originData`, and
  `metadata` only according to the governing codec or interface. They are not
  interchangeable generic byte names.

### 6.5 Solidity events

Events use PascalCase and describe a fact that occurred:

- `<Entity>Registered`, `<Entity>Removed` for membership changes;
- `<Value>Updated` for replacement, preferably including old and new values when
  the boundary needs both;
- `<Workflow>Started`, `Completed`, `Cancelled`, or `Failed` for exact
  transitions;
- `<Transfer>Sent` / `Received` when direction matters;
- standard event names exactly as the standard defines them.

Event parameters use lowerCamelCase and preserve domain roles. Indexed fields
are selected for query semantics, not because their names look important.

Renaming an event or changing its parameter types/order changes its signature
and topic. Treat it as a versioned external change.

### 6.6 Solidity custom errors

- Errors are PascalCase causes: `InvalidAmount`, `RemoteBridgeNotSet`,
  `UnauthorizedRemoteBridge`.
- Include the failed concept and useful typed context in parameters.
- Prefix an error with the contract/domain name only when a shared namespace,
  inherited surface, or consumer needs disambiguation. Do not mechanically
  prefix every local error.
- Preserve standard and inherited error names exactly.
- Do not use revert strings for a new project-owned error when a custom error can
  name the cause and data.

### 6.7 Solidity tests, mocks, harnesses, and scripts

Use these molds for new tests:

```text
test_<Behavior>_<Condition>_<Outcome>
test_RevertWhen_<Condition>
testFuzz_<Property>
invariant_<Property>
```

Omit a segment when it adds no information. Test contract names end in `Test`.
Focused files MAY use `<Subject>.<Concern>.t.sol` when the package already uses
that layout.

- Mocks use `Mock<Subject>`.
- Harnesses use `<Subject>Harness`.
- Invariant handlers use `<Subject>Handler`.
- Deployment scripts use `Deploy<Subject>` or the package's established
  `<Subject>Deploy` mold consistently.
- Configuration and operational scripts use an imperative name that states the
  action and direction.

Test-only names MUST use the same domain words as production. A mock may simplify
behavior; it may not silently rename the protocol concept.

## 7. Rust/Solidity boundary mapping

### 7.1 Precompile source of truth

For runtime precompiles, the canonical external ABI lives in:

```text
contracts/precompiles/src/I<Module>.sol
```

Rust `sol!` bindings, selectors, event decoders, and generated ABI output follow
that source. Do not repair a generated Rust name directly. Change the Solidity
source only after compatibility has been decided.

### 7.2 Conversion rules

Cross-language conversion may change only the mold, not the concepts:

| Solidity              | Rust                                           |
| --------------------- | ---------------------------------------------- |
| `registerValidator`   | `register_validator`                           |
| `proposalId`          | `proposal_id`                                  |
| `AuctionStage`        | `AuctionStage`                                 |
| `ValidatorRegistered` | `ValidatorRegistered` binding/event type       |
| `getBidsCount`        | a wrapper retaining `bids` + `count` semantics |

Rules:

- Keep the same domain nouns on both sides.
- Keep enum member order/discriminants 1:1 when the ABI maps an enum to Rust.
- Keep integer widths, signedness, array cardinality, and tuple field meaning
  explicit.
- A Rust newtype may improve internal safety, but boundary conversion must state
  exactly which ABI primitive and validation rules it represents.
- Function renames change selectors. Event renames change topics. Struct field
  renames can break generated clients even when tuple encoding is unchanged.
- Wire/body types carry an explicit version when compatibility requires it, for
  example `*BodyV1`. Do not append a version merely to postpone a naming
  decision.

### 7.3 External names

Keep names owned by Ethereum, OpenZeppelin, Reth, Commonware, ERC/EIP
specifications, and integrated protocols exactly as their APIs define them.
Project wrappers MAY adapt casing to Rust, but MUST NOT expand, translate, or
replace a standard term unless the wrapper intentionally exposes a different
project-level concept.

## 8. Compatibility and migrations

### 8.1 Classify the boundary

A rename is not equally safe at every boundary.

| Boundary                           | Rename consequence                                             |
| ---------------------------------- | -------------------------------------------------------------- |
| Local Rust/Solidity implementation | usually a local compile-time change                            |
| Public Rust API                    | downstream imports and call sites change                       |
| Storage schema or key              | persisted layout, lookup, or migration may change              |
| JSON/RPC/CLI/environment           | clients, scripts, and operations may break                     |
| Solidity function                  | selector and generated client API may change                   |
| Solidity event                     | event signature/topic and indexers may change                  |
| Wire/CE/protobuf encoding          | readers, writers, hashes, and version compatibility may change |
| External standard                  | conformance may be lost even if the new name looks clearer     |

Before changing a public or persisted name, the author MUST identify every
affected boundary. A source-level rename does not imply that the serialized,
persisted, selector, topic, or wire name may change with it.

### 8.2 Choose an explicit migration

For each compatibility-controlled boundary, choose one of:

- compatibility alias or serialization rename attribute;
- deprecation period with old and new entrypoints;
- explicit storage/data migration;
- new schema, ABI, or wire version;
- deliberate breaking release with downstream coordination.

Update the source interface or generator input rather than generated output.
After migration, search for stale synonyms and verify the relevant compiler,
tests, ABI diff, schema migration, and client-facing documentation.

### 8.3 Preserve concepts across boundaries

Semantic parity across languages is more important than identical casing.
`register_validator`, `registerValidator`, and `ValidatorRegistered` may be the
correct Rust command, Solidity command, and event forms for one concept.
Replacing `validator` with a different noun on one side is not a casing
conversion; it is a vocabulary change and requires semantic evidence.

## 9. Agent checklist

### 9.1 Workflow for creating or changing a name

Every naming change MUST follow this sequence:

1. **Set scope.** Identify the crate/contract and bounded context. Exclude
   generated and vendored sources.
2. **Find the owner.** Locate the specification, source interface, domain module,
   or protocol component that owns the concept.
3. **Inspect behavior.** Read the definition, meaningful callers, readers,
   writers, tests, side effects, and returned data shape.
4. **Write the verified meaning.** One sentence, without using the candidate
   name to prove itself.
5. **Select concepts.** Identity, role, lifecycle, direction, units,
   cardinality, and effects that a reader must predict.
6. **Select words.** Reuse words from the owner or governing standard. Record
   lexical provenance for every non-obvious word.
7. **Select the mold.** Apply the Rust or Solidity casing and grammar in this
   file.
8. **Check collisions.** Ensure the candidate does not erase a distinction or
   create a second term for an existing concept.
9. **Classify boundaries.** Local, Rust API, storage, JSON/RPC/CLI, ABI, event,
   wire, or standard-owned.
10. **Choose migration handling.** Alias, deprecation, migration, version, or
    deliberate breaking change.
11. **Update the whole safe surface.** Code, tests, docs, source schemas, and
    callers; never generated output alone.
12. **Verify.** Search for stale synonyms and run focused formatting, tests, and
    type/ABI checks appropriate to the boundary.

Use this record for a non-trivial proposal:

```text
Concept owner:
Verified meaning:
Current name:
Candidate name:
Lexical provenance:
Language mold:
Affected boundaries:
Compatibility plan:
Uncertainty:
```

If `Lexical provenance` is empty, the candidate is not canonical. Label it
**new vocabulary** or **uncertain** and request a domain decision.

### 9.2 Review checklist

Before accepting a new or changed name, verify:

- [ ] The name predicts the implemented behavior and side effects.
- [ ] Singular/plural form matches cardinality.
- [ ] Predicate grammar matches a boolean result.
- [ ] Identity, key, handle, hash, commitment, proof, and signature are not
      conflated.
- [ ] State, status, stage, kind, and type are not used as synonyms.
- [ ] Units, scale, currency/asset, time basis, and rounding are recoverable.
- [ ] Actor, authority, ownership, and source/destination roles are truthful.
- [ ] The words come from the governing standard or bounded context.
- [ ] Acronyms and proper names use the repository's established spelling.
- [ ] Rust or Solidity casing matches the symbol kind.
- [ ] The name does not encode a container or primitive type unnecessarily.
- [ ] Generated, vendored, fixture, and snapshot code did not define the canon.
- [ ] Public, persisted, serialized, ABI, event, and wire compatibility was
      classified explicitly.
- [ ] Cross-language names preserve the same concepts.
- [ ] Tests and documentation use the production owner's vocabulary.
- [ ] A rename is not being used to conceal an unresolved responsibility split.

When any semantic or lexical claim remains unverified, do not guess. Preserve
the existing compatibility-controlled name and record the candidate as
uncertain until the owner supplies the missing decision.
