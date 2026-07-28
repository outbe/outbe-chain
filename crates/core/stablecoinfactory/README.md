# Stablecoin Factory

Consensus state and typed internal API for Stablecoin Factory V1.

The Factory owns permanent token identity and the pending reservation triple:
token id, canonical ticker, and predicted address. Its public EVM surface is
read-only; Vote is the only creation adapter. Stablecoin V1 is available from fresh
genesis at chain protocol version `0`.

An admitted proposal reserves all three identities atomically. Approval initializes
one zero-supply ledger, installs the exact native marker, commits the registry and
emits `StablecoinCreated`. Expiry releases the reservation. A typed execution
`Error` rolls back target effects and retains the reservation for a future
validator-approved transition outside V1.

Factory registration means protocol admission only. It is not evidence of
backing, redeemability, price stability, issuer solvency, creditworthiness,
fee-asset eligibility, or payment-lane eligibility.
