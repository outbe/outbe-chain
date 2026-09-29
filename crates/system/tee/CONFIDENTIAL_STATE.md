# Synchronous confidential collateral (RIGHTS-20)

This is a fresh-genesis storage and ABI change. Credis, token transfers, Gratis,
Fidelity, aggregates, Promis capacity and events commit in the same EVM
transaction. Credis and Gratis factory entry points own an outer storage checkpoint, including
cross-module calls that do not enter through the ABI. There is no deferred work
queue or separately committed enclave economic database.

## State and execution

Gratis and Fidelity have independent keys and journals, at their existing
precompile addresses. Each journal uses slots 2–5: record count, current hash,
`mapping(uint64 => bytes)` records, and `mapping(uint64 => bytes32)` prefix hashes.
Gratis retains aggregate supplies at slots 0 and 1. Fidelity retains its global
qualification anchor at slot 1. Source addresses, balances, owner nonces, pending
notes, live allocations and cohorts are resolved only inside the enclave.

A request names the two committed journal heads. On a cache miss the host reads
sequential pages of at most 15 records (within the Noise frame limit). These
accesses depend on the global journal cursor, never on a source account. The
enclave authenticates each record against its domain, network and preceding head.
A record seals the completed state change and its full request hash. Fidelity
records encode an already accepted cohort change; recovery materializes it without executing money
movement, authorization, events or external calls again.
Recovery retries are bounded; repeated cache loss returns an error without
mutating authoritative state.

Each changed domain appends exactly one padded 4096-byte ciphertext record.
Gratis records contain updated account state plus optional note/allocation
changes. Fidelity records contain an accepted acquisition or LIFO reduction,
with its original amount and timestamp, so their size does not reveal a source's
cohort history. The chain stores no account-keyed private-state blobs.

The enclave's thread-local caches advance only from supplied journal records.
Prepared responses never commit the cache. A bounded 64-record undo history per
domain handles recent rollbacks; older historical states and cold starts rebuild
from journal pages. There are no on-disk economic checkpoints in this version.
Cold recovery remains proportional to journal history; optional authenticated
snapshots can reduce that cost without becoming a second authority.

Recovery reads rebuild a local cache without charging transaction gas or changing
EVM access warmth. They still read current journaled state, including writes earlier
in the transaction. Normal head reads and journal writes remain metered. Real-EVM
tests require identical cold/warm results and gas, and verify recovery leaves a
previously cold storage slot cold. Cold recovery time is node overhead, proportional
to history, rather than a caller gas charge that varies between validators.

Requests and responses are bound by a canonical hash and a verified local enclave
attestation. The host rechecks committed heads before persistence. Deterministic
encryption uses a keyed synthetic IV over the padded plaintext and context,
derives a distinct ChaCha20-Poly1305 key from that IV, and authenticates the
context. Divergent execution after rollback therefore does not reuse an AEAD
key/nonce for different plaintext. A Rust/TypeScript vector pins this format.

## Allocation authorization

Activation consumes a pending note once and creates one allocation for the exact
Credis ID. An internal `collateral_id` replaces the sealed owner field and is
omitted from public position getters. It cannot be reassigned, topped up or
reopened. Closed allocations retain a source-free tombstone in the materialized
state; historical encrypted records remain part of the chain history.

`CollateralAction` is `Return` or `Burn`. Execution derives `amount` from the
actual Credis settlement/forfeiture and passes its previous collateral as
`expected_remaining`. The enclave validates the Credis binding, exact expected
remaining amount, positive debit and allocation limit before updating the source.
A repeated authorization fails because remaining collateral strictly decreases.
There is no allocation sequence or redundant after-balance. Journal counts only
identify storage versions; owner modify nonces still authenticate owner writes.

The consensus runtime authorizes the payment/expiry transition. This design does
not independently prove ERC20 execution against a malicious runtime. Signed
transaction replay rules remain in force; the allocation check additionally
catches duplicate internal application and incorrect record selection. False
ERC20 return values and same-position reentrancy revert the outer transaction.

## Client changes and privacy boundary

`pledgeGratis` returns/emits an 80-byte owner-encrypted `pledgeReply`. The owner
opens it with its Gratis view key. `issueCredis` accepts one fresh X25519-encrypted
credential containing the network, note, destination and spend authorization.
Neither the note nor a source-bearing ciphertext is used as an issuance storage
key. `balanceOf` and `pledgedOf` return 112-byte root-bound encrypted views for
all accounts, including absent/zero accounts. Every view changes with the global
Gratis root, preventing account discovery by comparing ciphertext updates.
Rust helpers live in `outbe_tee::confidential`; MCP helpers live in `crypto.ts`.

The source is hidden during collateral execution, private storage access and
balance queries. **Existing public OCOMP owner/league snapshots are preserved by
explicit scope choice. They, public amounts and other public economic activity
can still support correlation; this is not a claim of complete unlinkability.**
Owner-authorized Fidelity queries and runtime league consumers keep their existing
disclosures. TEE CPU/cache/page-fault side channels are outside this boundary.

## Validation and scaling

Regressions cover cross-Credis substitution, A=100/B=900 with an invalid A=150
return, repeated application, closed allocation recovery, stale responses,
divergent rollback, equal host storage traces for different sources, and atomic
rollback across Credis/Gratis/Fidelity/Promis writes and events. Recovery tests cover
duplicate pages, cache loss between pages, tampering, reordered records and wrong
domains/networks. Real-EVM issuance and settlement tests exercise the credential
path and verify token, position, private-state and event rollback on failed ERC20
transfers/approvals and vault deposits.

Run the isolated scaling check with:

```sh
SOURCE_DATE_EPOCH=0 cargo test --locked -p outbe-tee-enclave --lib \
  confidential_ledger::tests::journal_scaling -- --ignored --nocapture
```

A local unoptimized macOS run measured the following (101 warm operations,
median; excludes transport, SGX transitions and EVM storage gas; other workspace
checks were running):

| Allocations | Warm return | Cold journal recovery | Ciphertext history |
| ---: | ---: | ---: | ---: |
| 1 | 115 µs | 4,650 µs | 4,096 bytes |
| 1,000 | 110 µs | 376,978 µs | 4,096,000 bytes |
| 10,000 | 117 µs | 3,795,378 µs | 40,960,000 bytes |

A return writes 4096 ciphertext bytes; a burn writes 8192 across both journals,
plus storage metadata and public effects. These are compute/scaling measurements,
not production SGX throughput claims. Fidelity evaluation retains its existing
cost proportional to the selected owner's cohort history.
