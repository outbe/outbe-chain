# Stablecoin

Rust-native Stablecoin V1 ledger shared by every Factory-registered token address.

Each address owns isolated balances, allowances, supply, roles, pause/freeze state,
EIP-2612 nonces and one shared Policy Registry binding. The public surface implements
ERC-20, EIP-2612, Final ERC-7943 and fixed `bytes32` memo variants. Initialization is
available only through the typed Factory API and always starts at zero supply.

Factory admission does not prove backing, redeemability, price stability, issuer
solvency, fee-asset eligibility or payment-lane eligibility.
