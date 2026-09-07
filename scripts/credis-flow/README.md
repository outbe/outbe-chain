# Credis User-Flow Demo (`examples/credis-flow`)

End-to-end TypeScript scripts that drive the Credis system on the Outbe chain. Each
file under `src/` is a standalone runnable that exercises one step of the user / CCA
flow - from pledging Gratis to settling the Credis position and unpledging.

### Confidential (TEE) Gratis/Credis design

These scripts target the confidential Gratis/Credis interfaces after the TEE
migration. There is no ZK pool: per-account Gratis balances and pledged amounts
are **encrypted at rest** and only the SGX enclave (and the account's view-key
holder, client-side) can read them.

- **Keys.** `src/confidential.ts` fetches an account's enclave-derived **view key**
  (decrypts its balance ciphertext) and **modify key** (authorizes writes) via the
  `outbe_deriveGratisKeys(account, ephemeralPubkey)` RPC, then decrypts / MACs
  byte-for-byte against the enclave (`bin/outbe-tee-enclave/src/gratis.rs`).
- **Reads** (`balanceOf`/`pledgedOf`) return the account's ciphertext blob; scripts
  decrypt it with the view key. `opNonceOf(account)` returns the write counter.
- **Writes** carry `(mac, opNonce)`:
  `pledgeGratis(amountStables, asset, maxGratis, mac, opNonce)` returns a
  `pledgeHandle`; `unpledgeGratis(amountStables, handle, mac, opNonce)`;
  `mineCoen(amount, mac, opNonce)`. Its input `amount` is protocol-6 GRATIS; its
  return value and `CoenMined.amount` are native-18 COEN. `mac = HMAC(modifyKey,
  op || amount || opNonce || chainId)` and `opNonce` must equal
  `gratis.opNonceOf(account)`.
- **The loan is priced at pledge time.** You name the *credit* you want
  (`amountStables` of `asset`), not the collateral: the chain converts it to gratis at
  the COEN/840 oracle rate and seals the stables amount, the asset and the rate into
  the encrypted pledge ticket. `maxGratis` caps the derived cost (the MAC only covers
  `amountStables`, so this is the slippage guard - and it is authenticated by your
  transaction signature). The gratis actually charged comes back on the
  `GratisPledged` event.
- **Credis.** `requestCredis(smartAccount, pledgeHandle, spendAuth, referenceCurrency)` (payable - the CCA
  attaches COEN equal to the pledged collateral) - called by the
  CCA. The user hands it a `pledgeSecret` (`HMAC(modifyKey, handle)`); the CCA binds
  it to the bundle with `spendAuth = HMAC(pledgeSecret, "credis-bind" || bundle)`.
  Neither the asset nor the amount is calldata - both are read back out of the ticket,
  so the loan is issued at the price the user accepted rather than a fresh quote.
  `settle(positionId, amount)` applies a payment interest first and principal
  second, and **automatically** releases the collateral share proportional to the
  principal it covered back to the pledger's encrypted balance - no reclaim note,
  no separate unpledge step.

Crypto uses Node's built-in `crypto` (HKDF-SHA256, HMAC-SHA256, ChaCha20-Poly1305)
plus `@noble/curves` for X25519. `npm run generate-types` stages the ABIs and runs
typechain; `npx tsc --noEmit` is clean.


### First-run quickstart

```bash
cd scripts/credis-flow
npm install
npm run generate-types
cp .local-reth.env.example .local-reth.env   # then fill values
npm run info                                  # read-only state snapshot
```

`npm run generate-types` reads JSON ABI files and emits `src/contracts/`.

## Configuration

Each script reads two env files from the project root, selected by the `envName`
CLI argument (default: `local-reth`):

- `.${envName}.env` - RPC URL, private keys, fixed addresses
- `.${envName}.deployment.env` - addresses produced by the Foundry deploy scripts

## Running

All scripts accept `[envName]` as an optional last positional argument. Each prints
state before / after and a `CHANGES` summary.
See all available scripts and their order in the `package.json`.
