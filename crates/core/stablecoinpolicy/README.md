# Stablecoin Policy Registry

Shared bounded account-eligibility policies for Stablecoin V1.

The fixed registry owns permanent policy ids, Whitelist, Blacklist and closed-depth
Directional policies, bounded membership batches and two-step administration.
Authorization queries are deterministic O(1) reads. Policy state does not contain
balances, freezes, reserves, issuer credentials or legal-compliance evidence.
