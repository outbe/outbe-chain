# Outbe Txpool

`outbe-txpool` is the node-side Reth transaction pool integration for Outbe.
It exists so txpool admission and ordering policy are not hidden inside
`outbe-node` startup wiring.

This crate does not define which transactions are free. That policy lives in
`crates/system/zerofee`. This crate only asks that registry whether a signed EVM
transaction is a zero-fee candidate, whether it is authorized against current
state, and whether it receives a reserved txpool priority class.

## Code Map

- `crates/system/zerofee`: source of truth for hook classification and state
  authorization.
- `crates/blockchain/txpool`: Reth txpool builder, admission wrapper, and
  ordering class.
- `crates/blockchain/evm`: block execution check that repeats authorization and
  waives native fee debit.
- `bin/outbe-feeder`: creates the signed `Oracle.submitVote(...)` transaction.

## Transaction Model

Zero-fee transactions are still normal signed EVM transactions:

- They enter through public transaction submission.
- They keep normal signature, nonce, tx type, calldata, and gas-limit checks.
- They execute through the EVM and the target precompile.
- They count gas against the block gas limit.
- Only native fee debit is waived, and only after executor authorization.

The current zero-fee hook is `OracleSubmitVote`. The candidate transaction shape
is defined by `crates/system/zerofee/src/oracle.rs`:

- `to == ORACLE_ADDRESS`
- calldata starts with `IOracle.submitVote(...)`
- calldata ABI-decodes successfully
- `max_priority_fee_per_gas == Some(0)`
- `max_fee_per_gas >= MIN_PROTOCOL_BASE_FEE`
- `value == 0`
- calldata size is at most `MAX_ZERO_FEE_ORACLE_CALLDATA_BYTES`
- gas limit is at most `MAX_ZERO_FEE_ORACLE_GAS_LIMIT`

A paid oracle vote is still valid, but it is not a zero-fee candidate: if
`max_priority_fee_per_gas` is nonzero, `classify` returns `Ok(None)` and the
transaction follows the normal fee path.

Malformed zero-fee-shaped transactions are rejected instead of silently falling
back to the paid path. Examples: too-low fee cap, nonzero value, oversized
calldata, excessive gas limit, or calldata that matches the selector but cannot
decode.

## Admission

`OutbePoolBuilder` creates the Reth Ethereum transaction validator with balance
checking disabled, then wraps the validation result in
`OutbeTransactionValidator`.

That is intentional:

1. Reth still performs its normal non-balance validation.
2. A gasless validator feeder may have zero native balance, so Reth's native
   balance check would reject the transaction before Outbe can inspect the
   zero-fee hook.
3. `OutbeTransactionValidator::apply_outbe_policy` restores the balance rule for
   every non-zero-fee transaction.

Admission behavior:

- If the inner Reth validator rejects the transaction, Outbe returns that result.
- If `zerofee.registry().classify(tx)` returns `Ok(None)`, Outbe checks
  `tx.cost() <= signer_balance`. If not, it returns Reth `Overdraft`.
- If `classify(tx)` returns `Err`, Outbe rejects the transaction as invalid.
- If `classify(tx)` returns `Ok(Some(candidate))`, Outbe reads latest state and
  calls `authorize_fee_waiver(candidate)`.
- If authorization succeeds, Outbe returns the transaction as valid with pool
  balance allowance `U256::MAX`.
- If authorization fails, Outbe rejects the transaction as invalid.

For `OracleSubmitVote`, authorization checks:

- signer is an active validator or delegated feeder
- validator exists in `ValidatorSet`
- validator status is active
- validator has a BLS share
- validator has not already voted in the current oracle period

The txpool state check is an admission check, not the final authority. State can
change after pool admission, so the block executor must repeat authorization.

## Ordering

Ordering is implemented by `OutbeTransactionOrdering`.

The priority value is `(class, tip)`:

- normal transactions: `(0, effective_tip_per_gas)`
- `OracleSubmitVote` zero-fee candidate: `(1, 0)`
- malformed zero-fee marker: `Priority::None`

Only hooks explicitly listed in `zero_fee_priority_class` receive a reserved
class above the normal fee market. Today the only such hook is:

- `ZeroFeeHookId::OracleSubmitVote -> Some(1)`

This is deliberately exhaustive. When a new `ZeroFeeHookId` is added, the code
must decide whether that hook gets reserved priority or falls back to normal
ordering. A future gasless hook does not automatically outrank fee-paying
transactions.

Ordering uses classification only. It does not read state. That is acceptable
because admission already authorized the candidate, and execution repeats the
state authorization before the fee waiver is applied.

## Execution Contract

The txpool cannot make a transaction free by itself. The executor is the final
authority.

During block execution, `crates/blockchain/evm`:

1. Builds the same `ZeroFeeTransaction` view from the recovered signed
   transaction.
2. Calls `zerofee.registry().classify(tx)`.
3. If it is not a candidate, executes the normal EVM transaction path.
4. If it is a candidate, calls `authorize_fee_waiver(candidate)` against the
   in-block state.
5. If authorization fails, rejects execution.
6. If authorization succeeds, sets the EVM tx environment gas price and priority
   fee to zero for that transaction, executes it through the normal EVM path, and
   restores the EVM config afterwards.

This means a zero-fee oracle vote still executes `Oracle.submitVote(...)`; it
does not bypass the Oracle precompile or create a second executor path.

## Guarantees And Limits

Guaranteed:

- Normal transactions still require enough native balance.
- Paid oracle votes remain normal paid EVM transactions.
- Gasless `submitVote` requires both txpool admission authorization and executor
  authorization.
- Only `OracleSubmitVote` currently receives the high-priority txpool class.

Not guaranteed:

- Priority does not guarantee inclusion if the transaction is invalid,
  nonce-blocked, dropped, state-invalidated, or cannot fit under block gas.
- Txpool authorization does not guarantee executor authorization, because state
  may change between admission and payload execution.
- Zero-fee does not mean zero gas accounting. It only waives native fee debit.

## Review Checklist

Use this checklist when deciding whether the code matches the intended behavior:

1. `outbe-node` wires `OutbePoolBuilder` from this crate into the node stack.
2. The inner Reth validator is built with balance checks disabled.
3. `OutbeTransactionValidator` restores balance checks for non-zero-fee
   transactions.
4. Zero-fee candidates are authorized through `crates/system/zerofee`, not
   through ad hoc txpool logic.
5. Only `OracleSubmitVote` maps to reserved priority `(1, 0)`.
6. A malformed zero-fee marker gets `Priority::None` and invalid admission.
7. The executor repeats the same authorization and zeroes fee fields only for the
   authorized transaction execution.

## Pending staleness eviction

A transaction the network cannot mine must not live in the pool forever. Two
independent bounds enforce that, and both are node-local pool policy: neither
affects block validity, because payload content is proposer-discretionary and
block validation audits only parent binding, beneficiary and system-transaction
layout.

### 1. Parked lifetime (upstream mechanism, hardened defaults)

Reth ages out parked (nonce-gapped, underpriced) transactions after
`--txpool.lifetime`. Outbe changes three defaults, installed before CLI parsing
so explicit operator flags still win:

| Setting | Reth default | Outbe default | Why |
|---|---|---|---|
| `--txpool.lifetime` | 3 h | 120 s | Two-second blocks; a parked transaction that is still parked after two minutes is not going to be mined. |
| `--txpool.no-locals` | off | on | RPC-submitted transactions are otherwise exempt from lifetime eviction — exactly the traffic that must be evictable on a public endpoint. |
| `--txpool.disable-transactions-backup` | off | on | A restart must not resurrect transactions the node deliberately evicted. |

### 2. Pending staleness (this crate, `maintain.rs`)

Reth bounds only the parked sub-pools; a *pending* transaction has no lifetime
at all. It is therefore re-selected by every payload build and, when a proposal
containing it fails to finalize, re-injected by the reorg path — indefinitely.

`maintain_outbe_pool` subscribes to the canonical-state stream (alongside the
upstream maintenance task, which keeps owning nonce/balance pruning and reorg
handling) and applies a two-snapshot rule:

1. At most once per `--txpool.outbe.pending-staleness-secs` of **canonical block
   time** (never wall clock), snapshot the pending-transaction hash set.
2. A hash present in two consecutive snapshots has stayed pending for at least
   one full interval without being mined: evict it, together with its
   descendants (same-sender higher nonces, unincludable behind it anyway).

Effective pending lifetime is therefore one to two intervals. Mined or dropped
transactions simply vanish from the next snapshot; a re-added transaction (gossip
or reorg re-injection) starts a fresh cycle and is evicted one interval later.
Out-of-order tip timestamps — normal during pre-finalization head switches —
never evict early.

The default interval is 600 s. Keep it uniform across the fleet: a stuck
transaction is shed at the rate of the slowest-configured node, since any node
still holding it can propose it.

Every eviction is logged, never silent:

```
WARN outbe::txpool tx_hash=0x… sender=0x… nonce=… reason=stale_pending
     staleness_interval_secs=600 evicting stale pending transaction
```

`reason` is `stale_pending` for the transaction itself and
`descendant_of_stale` for same-sender successors removed with it. Metrics:
`outbe_txpool_stale_evicted_total` (counter) and
`outbe_txpool_pending_snapshot_size` (gauge).

### What eviction does and does not do

Eviction bounds pool residency. It does not touch chain state, and the
distinction matters when reasoning about a stuck sender:

- **The transaction never stays forever.** Pending: one to two staleness
  intervals. Parked (nonce gap, underpriced): the 120 s lifetime above. Both
  paths apply to RPC-submitted transactions.
- **The sender's nonce stays unconsumed.** A nonce is consumed only by
  executing the transaction in a block. The pool cannot execute it, cannot sign
  a replacement on the sender's behalf, and must not pretend locally that the
  nonce advanced — that would diverge from chain state. Only the key holder can
  close the gap, by submitting something at that nonce.
- **Successors do not pile up.** Transactions the sender already sent at higher
  nonces sit in the queued sub-pool behind the gap and age out on the same
  120 s lifetime.
- **Residual risk.** The sender (or anyone replaying the signed bytes) can
  resubmit the same transaction; it is admitted again and evicted again after
  one interval. That bounds each episode to roughly one staleness interval
  instead of leaving it resident indefinitely, but it does not make repeated
  submission free for the network. Bounding per-transaction validation cost is a
  separate concern from pool residency and is not solved here.

## Tests

Focused txpool tests:

```bash
cargo test -p outbe-txpool
```

Cross-module zero-fee tests:

```bash
cargo test -p outbe-zerofee -p outbe-feeder -p outbe-evm -p outbe-txpool -p outbe-node
```

Release build used for local validator runs:

```bash
cargo build --release -p outbe-chain -p outbe-feeder
```
