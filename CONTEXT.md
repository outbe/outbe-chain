# Ubiquitous language

## Architecture Space

One of the three top-level responsibility boundaries of Outbe Chain. A space is
defined by what it makes true, not by the current filesystem location of a crate.

### Blockchain Space (`B`)

The network and execution substrate that makes replicated protocol execution
possible: node lifecycle, consensus/finality, block execution, EVM, transaction
pool, RPC and authenticated persistence/projection infrastructure.

### System Space (`S`)

Network-wide system mechanisms and policies used to operate and evolve the chain:
validator lifecycle and economics, scheduling, accounting/emission, Oracle, TEE,
governance/update, fee policy and shared cryptographic verification services.

## Executable Governance

### L2 Registry Proposal

An active-validator Vote proposal targeting `L2Registry` with one strict JSON
mutation (`register` or `setZkEnabled`). The proposal stays pending
through the standard voting window; a two-thirds yes quorum applies the mutation
atomically during post-deadline begin-block tally.
_Avoid_: direct L2 registration, Governance OIP/GIP

### Registry Mutation

An L2Registry-owned state transition. Admission and ZK policy changes are applied
by its compile-time Vote target. A registered `l1Address` owner may remove only
its own network through `removeNetwork`; no public register or set selector exists.
_Avoid_: permissionless registry update, validator-owned ABI write

### Core Space (`C`)

The Consume-to-Gain protocol and its business state: Tribute, Nod, Gratis,
Metadosis, AgentReward, Lysis and the product/value modules that evolve from them.

## Protocol Flow Specification (PFS)

An end-to-end outcome contract crossing multiple Architecture Spaces or module
authorities. It references ADRs but does not own or redefine their local state.

## ADR identity

`ADR-<space>-<module>-<sequence>`, where space is `B`, `S` or `C`, module is a
stable three-letter architectural-owner code registered in `docs/adr/index.md`, and
sequence is dense in dependency/evolution order within that `(space, module)` pair.
The number is not global and does not imply nonexistent ADRs in other modules.

## Off-chain Computation

### OCOMP Operational Kernel

The network-wide mechanism that owns finalized computation-job lifecycle,
process/evidence boundaries and certified dispatch without owning a domain
program's business semantics or effects.
_Avoid_: Lysis framework, arbitrary task runner

### Typed Program

A closed, fork-pinned domain computation with its own authenticated input,
deterministic semantics, typed result verifier and private effect authority.
_Avoid_: plugin, uploaded job, task adapter

### Job Intent

The consensus record that fixes one complete logical computation, its authenticated
input commitments, attempt, request budget split, activation preconditions,
protocol interpretation and deadline before off-chain execution begins. It is
the parent of all local work shards, not one worker-sized slice. Its population
is committed by counts/roots and has no artificial PoC total-size ceiling.
_Avoid_: event, worker job, scheduler task

### Job Registry

The durable validator-local index of independently progressing OCOMP Job
attempts. Each entry preserves one exact candidate/finalized identity and
references its authenticated input lease; an operational concurrency limit may
apply, but one retained or failed Job never replaces or blocks another.
_Avoid_: current job slot, predecessor slot, global OCOMP lock

### Authenticated Input Lease

A durable, content-addressed retention obligation for one exact set of
authenticated source commitments. Several Job attempts may reference the same
lease only when those commitments are byte-identical; the lease is released
only after every referencing Job has reached its retention gate.
_Avoid_: JobId, live database lock, temporary file

### Work Shard

A deterministic bounded slice of one Job Intent that workers may execute and
retry independently before the complete result is reduced. Reaching the shard
capacity creates the next shard; it never rejects the next valid input.
_Avoid_: Job Intent, partial activation, validator vote

### Authenticated Input Bundle

A content-addressed export whose canonical manifest and objects reconstruct the
finalized commitments named by one Job Intent.
_Avoid_: database dump, trusted Mongo snapshot

### Validator Domain

One Byzantine evidence identity comprising a validator's node, OCOMP services,
workers and local artifacts, regardless of worker or process count.
_Avoid_: worker vote, process validator

### Full-result Vote

One validator domain's canonical on-chain submission containing the complete
constant-size Typed Program result plus its validator identity and signature.
The vote never contains population-sized output records.
_Avoid_: digest announcement, worker result, activation request

### Quorum Apply

The atomic consensus transition performed by the third matching Full-result
Vote. It records quorum, derives a private program-scoped capability and commits
the owning domain effects or none; no later activator transaction exists.
_Avoid_: relay activation, permissionless activation, generic write batch
