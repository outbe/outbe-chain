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

The consensus record that fixes one computation request, its authenticated input
commitments, attempt, request budget split, activation preconditions, protocol
interpretation and deadline before off-chain execution begins.
_Avoid_: event, worker job, scheduler task

### Authenticated Input Bundle

A content-addressed export whose canonical manifest and objects reconstruct the
finalized commitments named by one Job Intent.
_Avoid_: database dump, trusted Mongo snapshot

### Validator Domain

One Byzantine evidence identity comprising a validator's node, OCOMP services,
workers and local artifacts, regardless of worker or process count.
_Avoid_: worker vote, process validator

### Certified Activation

The atomic consensus transition that verifies evidence for one exact typed
result, derives a private program-scoped capability and commits the owning domain
effects or none.
_Avoid_: generic write batch, result import
