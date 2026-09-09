# Smart accounts and bundles

The factory deploys Kernel v4 accounts independently of CCA registration:

```solidity
address account = factory.createAccount(owner, salt);
address predicted = factory.getAccountAddress(owner, salt);
```

Creation installs only the owner's SudoPolicy/ECDSASigner permission. The owner can
receive assets and execute ordinary transfers immediately. Prediction depends on
the owner, salt and deployed factory stack, with no CCA registry call.

To attach a bundle, obtain `getBundleInstallPackages(cca, tokens, senders)` and sign
Kernel's native `InstallPackages` EIP-712 message using the account's current root.
Use the account's `nonce(0)`, the Kernel domain (`Kernel`, `0.4.0`, chain ID, account),
and permission signature encoding `abi.encode(bytes[]{"", signature})` for the
initial owner root. Anyone can relay the exact signed configuration:

```solidity
factory.openBundle(account, cca, tokens, senders, nonce, signature);
```

Installation and custody registration are atomic. The CCA must be active, tokens
must be unique six-decimal contracts, and senders must be nonzero and unique. The
factory accepts only accounts it created. Each account can open exactly one bundle.

## Reserve custody

`BundleModulePlugin` holds reserves separately from the account. An allowed sender
calls `topUpFor(account, token, A)` directly after both sender and account approve
custody. Custody pulls A from each and credits 2A. Ordinary account funds remain
freely transferable. The account fallback `bundleBalance(token)` is a read-only
view; custody funding is never authenticated through account forwarding.

A purchase calls `spend(Payment, signature)` from the account. The CCA signs both
the UserOperation and the custody EIP-712 `Payment` (domain `OutbeBundle`, version
`1`). The latter binds account, token, recipient, amount, nonce, deadline, chain and
custody address. Custody independently checks this signature and active CCA status,
debits 2A, pays A to the recipient and releases A to the account. Execution enforces
a 1,000-token daily limit per account/token. Failed transfers roll back the reserve,
nonce and limit updates. Transfers must have exact balance deltas; fee-on-transfer
and rebasing tokens are unsupported.

## Closing and account mutation

The owner invokes `closeBundle()` through the account. Every configured token's
bundle balance must be zero. Closing permanently retires eligibility and removes
the CCA permissions, payment hook and bundle view. The owner permission remains.
Direct account calls to custody's `retire()` also permanently close eligibility;
installed modules can then be removed with the normal Kernel API.

While open, module installation/removal, root replacement and selector grants are
locked. Upgrades and arbitrary delegatecalls first require zero reserves and retire
eligibility permanently, even on an unopened account. The lifecycle record lives
in custody and cannot be reset by writing account storage. Kernel's internal
UserOperation self-dispatch continues to work. The small upstream overlays are
reproducible with `script/sync-guarded-kernel.py`; see `src/kernel/guarded/README.md`.

## Deployment and clients

Run the stack deployment scripts, then run `script/ConfigureBundleCustody.s.sol`
as VaultRouter's administrator with `BUNDLE_MODULE_PLUGIN_ADDRESS` set. VaultRouter
binds that custody address once. `requestCredis` rejects unopened/closed accounts or
a different linked CCA before consuming a pledge. VaultRouter withdraws into itself,
approves custody for exactly A, calls `topUpFor`, then clears approval.

The factory API and deterministic addresses change. Existing accounts and reserves
are not migrated by these scripts; deploy a fresh stack and configure the router
before issuance. A router already bound to custody cannot be rebound.

From `scripts/credis-flow`:

```bash
npm run top-up-sa -- local-reth
npm run open-bundle -- local-reth 1000
npm run request-credis -- local-reth
npm run cca-simulate-purchase -- local-reth
npm run close-bundle -- local-reth  # only after all reserves are spent
```

`open-bundle` also approves the specified matching contribution (default 1,000).
It does not move reserves; issuance triggers the matched deposit.

Validation: `forge test --root contracts/smart-account --offline`, targeted
`outbe-vaultrouter`/`outbe-credisfactory` Rust tests, and the
`outbe-evm --test vaultrouter_bundle` integration tests.
