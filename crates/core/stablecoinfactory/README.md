# Stablecoin Factory

Consensus state and typed internal API for Stablecoin Factory V1.

The Factory owns permanent token identity and the pending reservation triple:
token id, canonical ticker, and predicted address. Its public EVM surface is
read-only; Vote is the only creation adapter.

Factory registration means protocol admission only. It is not evidence of
backing, redeemability, price stability, issuer solvency, creditworthiness,
fee-asset eligibility, or payment-lane eligibility.
