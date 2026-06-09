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
