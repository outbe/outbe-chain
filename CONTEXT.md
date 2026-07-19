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
