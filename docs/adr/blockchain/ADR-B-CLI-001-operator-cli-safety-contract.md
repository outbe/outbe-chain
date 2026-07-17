# ADR-B-CLI-001: outbe-cli is a transaction-intent and operator-safety boundary

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `bin/outbe-cli`, its RPC client, signer, ABI commands and TEE
  join/encrypted-offer workflows
- **Depends on:** ADR-B-OCD-006, ADR-B-OCD-013, ADR-S-TEE-001, ADR-S-TEE-002, ADR-C-TRB-002, ADR-B-RPC-001, ADR-B-TXP-001
- **Supersedes:** The operator-CLI portion of the former pre-space RPC/operator placeholder

## Context

The CLI turns human input and local secrets into signed chain transactions and
enclave mutations. It is not merely presentation code: wrong chain, stale nonce,
unfinalized receipt, permissive attestation or ambiguous output can cause an
operator to believe a validator joined, a vote landed or an encrypted offer exists
when only a transaction hash was produced.

## Decision

Every mutating command follows one explicit intent lifecycle:

```text
Parsed -> PreconditionsRead -> IntentDisplayed/Confirmed -> Signed -> Submitted
       -> MinedSuccess -> Finalized -> PostconditionVerified
```

Commands must state the terminal level they reached. “Submitted” is never printed
as success. Safety-critical commands default to waiting for a successful receipt,
required finality and an authoritative postcondition; an explicit `--no-wait`
returns a machine-readable submitted result.

The CLI verifies expected chain id/genesis identity, target addresses, sender,
calldata summary, value, nonce and fee policy before signing. It uses typed ABI and
transaction libraries or byte-for-byte conformance tests for any custom codec.
Read commands declare latest/finalized/projection/proof semantics according to
ADR-B-RPC-001.

## Command and authority surface

The command tree covers validator registration/info, staking, rewards, epoch,
slashing, chain/monitoring, Oracle/delegation, Tribute queries/encrypted offers,
EIP-7702 ZeroFee authorization, TEE join/pubkey comparison and generic Vote
operations. Mutations require an ECDSA signer; reads do not.

The signer currently constructs EIP-155 legacy transactions, reads chain id,
`latest` nonce, gas price and estimate, adds gas buffers and submits raw bytes.
TEE/Tribute commands additionally connect to a local enclave, read registry keys,
encrypt an offer or ingest a sealed handoff. Those enclave commands are secret and
attestation boundaries governed by ADR-S-TEE-001 and ADR-S-TEE-002, not ordinary RPC helpers.

## Secret and signing policy

Private keys and passphrases must come from a permission-checked file, OS keyring,
hardware signer or protected interactive prompt. Secret values must not be accepted
in argv by default, logged, included in errors or retained beyond signing. The CLI
derives and displays the sender and refuses an unexpected account/chain unless the
operator explicitly confirms.

Nonce selection uses `pending` state or an explicit nonce and detects replacement/
concurrent submission. Fee mode is chain-aware; buffers have a displayed maximum
cost and cannot rely on an assertion that overpricing is free unless the exact
transaction is executor-authorized for ZeroFee.

## Receipt, finality and postconditions

Receipt polling is bounded per request and overall, verifies response/transaction
hash, status, target and expected event topics/data, then waits for the configured
finality boundary. Reorged receipts restart tracking. Postconditions use finalized
state or verified CE proof where applicable.

TEE join accepts only an `OfferKeySealed` log from the submitted successful
registration receipt, then verifies validator, key epoch, chain id and expected
on-chain offer public key before enclave ingestion. Tribute offer success verifies
receipt, derives the canonical owner/day id and confirms authenticated/finalized
presence; Mongo projection is a separate optional demonstration check.

## Transport, parsing and output

RPC has connect/request/overall deadlines, response-size limits, request id and
JSON-RPC version validation, TLS policy and retry classification. Reads may retry;
raw transaction submission retries only by querying the exact hash/nonce first.
Errors preserve stable categories without exposing secrets.

Every command supports stable JSON output containing chain id, command, sender,
intent, transaction hash, receipt/finality block and verified postcondition. Human
output is derived from the same typed result. Exit zero means the requested terminal
condition was achieved; timeout, revert, mismatch and partial submission are
distinct nonzero outcomes.

## Encrypted Tribute and TEE specifics

Encrypted offer construction uses cryptographic randomness and the exact registry
offer key/epoch. Its versioned AEAD envelope must bind chain, contract, sender,
epoch and public flags as specified by ADR-C-TRB-002. Sensitive plaintext is never printed.
The current empty-AAD format remains compatibility debt, not the desired contract.

Enclave connections use the chain's attestation policy. `dev_accept_any` is allowed
only when chain configuration explicitly declares a non-confidential development
mode and output prominently records that fact. A production command cannot
silently downgrade quote verification.

## Compatibility and production evidence

CLI flags/subcommands, JSON result schema, ABI addresses/selectors, transaction
type/codec, encryption envelope and exit codes are operator automation surfaces.
Breaking changes require versioning/deprecation. The CLI should query protocol
version/capabilities and reject unsupported command/chain combinations.

Evidence inspected includes the full command tree, RPC transport, legacy signer,
ABI definitions, Tribute encryption, TEE join/pubkey workflow and command/unit
tests. Existing tests heavily cover parsing, RLP helpers and mocked RPC reads; they
do not prove receipt/finality, real RPC deadlines, production attestation or
end-to-end postconditions.

## module audit profile

The CLI boundary should expose typed `Intent -> Outcome` workflows with secret
providers, chain identity and completion policy injected explicitly. Mutating
commands cannot bypass the common submit/receipt/finality engine. TEE workflows
must return typed attestation and handoff receipts rather than parse arbitrary logs.

## Consequences and rejected alternatives

Waiting by default makes demos and operations slower but removes the dangerous
equation of hash with success. Machine-readable `--no-wait` preserves automation
and bulk workflows. Passing raw private keys in argv was rejected as the normal
interface. Per-command bespoke transaction polling was rejected because it drifts
in finality, timeout and error behavior. Folding CLI into RPC ADR was rejected
because local secrets and external effects are a separate authority.

## Open questions and technical debt

- Remove/deprecate global `--private-key <hex>`: argv leaks through shell history
  and process listings. Add file/keyring/hardware/prompt providers with permission
  and account checks.
- `TxSigner` reads nonce at `latest`, not `pending`; concurrent commands can sign
  the same nonce. Add pending/explicit nonce management and replacement tracking.
- Most mutations return immediately after `eth_sendRawTransaction` and print only
  a hash. Add common successful-receipt, finality and postcondition verification.
- TEE join polls all matching validator logs from a starting block and takes the
  last one without binding it to the submitted tx/receipt or finality. A stale or
  unrelated handoff can be ingested.
- TEE `join` and `pubkey` unconditionally use `QuotePolicy::dev_accept_any()`.
  Load and enforce the chain's production policy; make any dev downgrade explicit
  and impossible on a production chain.
- RPC uses a default `reqwest::Client` with no explicit request deadline or
  response-size policy. A hung `eth_getLogs` call can defeat the outer TEE join
  timeout because elapsed time is checked only after the call returns.
- `poll_offer_key_sealed` suppresses every log RPC error with
  `unwrap_or_default`, turning outage/auth/malformed responses into an eventual
  generic timeout. Preserve typed failures and bounded retries.
- Legacy transaction building is hand-written and legacy-only. Adopt Alloy's
  canonical signer/envelope implementation, add low-s/chain/large-calldata vectors,
  and support the active fee transaction type deliberately.
- The 2x/1-gwei gas-price comment claims overpricing costs nothing because the
  chain is ZeroFee, but most CLI mutations are normal paid transactions. Display
  max cost, cap buffers and distinguish actual waived hooks.
- `eth_estimateGas` omits transaction `value`; value-bearing calls can be
  under/incorrectly estimated. Include the exact intent fields.
- Tribute offer uses a fixed 8,000,000 gas limit, submits without receipt checking
  and prints “Verify once mined”. Replace this with protocol capability/estimate
  policy and finalized authenticated postcondition.
- Version the Tribute encryption envelope and bind chain id, factory address,
  sender, offer-key epoch and public flags as AEAD associated data.
- Add stable `--json`, documented exit codes and structured partial outcomes.
  Current human strings are not a safe automation contract.
- Verify RPC URL scheme/host and chain/genesis identity before signing; localhost
  default is convenient but a supplied endpoint can silently target another chain.
- Add E2E tests for revert, dropped/replaced/reorged transaction, RPC outage and
  timeout, wrong chain/account, concurrent nonce, finalized receipt, TEE stale log,
  strict/dev attestation and Tribute proof/projection lag.
