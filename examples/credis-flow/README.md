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
- **Credis.** `requestCredis(smartAccount, pledgeHandle, spendAuth)` (payable - the CCA
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

> AUTH: `outbe_deriveGratisKeys(account, ephemeralPubkey, signature)` requires
> proof of control of `account` - an EIP-191 `personal_sign` over
> `"outbe/gratis/derive-keys/v1" || account || ephemeralPubkey`, which the node
> recovers and matches before asking the enclave. `deriveGratisKeys(signer)` in
> `confidential.ts` produces it, so you sign with the account key (read-only
> scripts like `0-info` therefore need `USER_PRIVATE_KEY` to decrypt balances).

Contract bindings come from this repo's own ABIs. `npm run generate-types` first
runs `scripts/prepare-abis.mjs`, which copies the required JSONs out of
`../../contracts/precompiles/abi-export/`
and `../../contracts/smart-account/abi-export/` into a local
`abi/` directory, then typechain generates ethers v6 factories into
`src/contracts/`. Both directories are gitignored and regenerated on every build.

The smart-account stack runs on **ZeroDev Kernel v4 / EntryPoint v0.9**. Because
Kernel v4 models the account owner as a *permission* (not a plain root validator),
owner and CCA UserOps use the permission nonce type and the Kernel v4
`PermissionSignature` (`abi.encode(bytes[])`) format - see the helpers in
`src/utils.ts` (`ownerPermissionId` / `ccaPermissionId` / `permissionNonceKey` /
`encodePermissionSignature`). Redeploy the v4 smart-account stack (new bytecode ->
new addresses) and regenerate the deployment env before running these scripts.

### First-run quickstart

```bash
cd examples/credis-flow
npm install
npm run generate-types
cp .local-reth.env.example .local-reth.env   # then fill values
npm run info                                  # read-only state snapshot
```

The `.local-reth.env` file is resolved relative to the project root by
`loadEnv(import.meta.url, "local-reth")` in `src/utils.ts`. Override the
environment name by editing `DEFAULT_ENV` at the top of `utils.ts`.

---


## Layout

```
src/
+-- 0-info.ts                   Print current state of all actors
+-- 0-setup-native.ts           Fund user + CCA with native COEN
+-- 0-setup-erc20.ts            Mint / move ERC20 into user + vault router
+-- 0-setup-gratis.ts           Mine seeded gem -> Promis -> confidential Gratis
+-- confidential.ts             Client-side TEE crypto (key fetch, decrypt, MAC)
+-- 1-pledge-gratis.ts          User pledges for N stables of credit -> pledge handle
+-- 1.1-unpledge-gratis.ts      Direct reclaim of an UNSPENT pledge (e.g. credis rejected)
+-- 2-top-up-smart-account.ts  Deploy smart account; transfer ERC20 into it
+-- 3-request-credis.ts         CCA calls requestCredis(handle, spendAuth); vault funds enter bundle balance
+-- 4-cca-simulate-purchase.ts  CCA uses bundle funds via per-token permission
+-- 4.1-user-sa-withdraw.ts     User withdraws their free (non-bundled) balance
+-- 5-user-settles.ts           User settles any amount (batched UserOp); the matching collateral share auto-unlocks
```

Collateral unlock is automatic on each `settle` payment (released to the pledger's
encrypted balance), so the old `6-user-unpledge-gratis.ts` reclaim step is gone.

A position has no installments, no due dates and no maturity: it is settleable from
the moment it opens, and the owner - or anyone paying on their behalf - may settle
any amount at any time.
Interest is not accrued per block; it is computed at settlement over the whole UTC
days elapsed, simple and ACT/365 on the outstanding principal, and is always
collected before any principal.

## Installation

```bash
cd examples/credis-flow
npm install
# Stage local abi/ from outbe-chain/contracts and run typechain
npm run generate-types
```

`npm run generate-types` reads JSON ABI files produced by `make export-abi` in the
parent project (`abi-export/*.json`) and emits `src/contracts/`.

## Configuration

Each script reads two env files from the project root, selected by the `envName`
CLI argument (default: `local-reth`):

- `.${envName}.env` - RPC URL, private keys, fixed addresses
- `.${envName}.deployment.env` - addresses produced by the Foundry deploy scripts

### Outbe Testnet Peira env

Add this to `.outbe-peira.env`:

```dotenv

export RPC_URL="https://peira-rpc.outbe.net"

# modules
export GRATIS_ADDRESS=0x0000000000000000000000000000000000001003
export GRATIS_FACTORY_ADDRESS=0x0000000000000000000000000000000000002003
export CREDIS_ADDRESS=0x000000000000000000000000000000000000100A
export CREDIS_FACTORY_ADDRESS=0x0000000000000000000000000000000000001009

# addresses and keys
export PRIVATE_KEY=8365107f4bd3e538431e7c8dcdd806b2eedba7ae095b846dc8eca0db18bb9b91
export OWNER_ADDRESS=0xDBf385DF0931F78B792A9D040758fc47Ea838386

export USER_PRIVATE_KEY=0xef902d357ec36a786a0c091442a6fc3ae7176e71f33203c533168549f8311b78
export USER_ADDRESS=0x64CCA861d30714593cB690e0a550C8a9b8b3b058

export ERC20_HOLDER_PRIVATE_KEY=0x4d9607c0fcf9d2aa80fb7600cbb2f4aa5d36281145f1103509cb62d3a48836b5

export CCA_PRIVATE_KEY=0x4d1e6508b6901e2dec9e65aeda66cfd4137013056d50c45742daa13fc73f928a
export CCA_ADDRESS=0xbb94B1816c439d84B1C0b43E56b05EE7f2eA0e35

export ERC20_ADDRESS=0x99142E5359d0492783751964eA1a500686538E8C
```

Add this to `.outbe-peira.deployment.env`:

```dotenv


# Kernel stack deployment at block 240467 timestamp 1779359920
export ENTRYPOINT_ADDRESS=0x0000000071727De22E5E9d8BAf0edAc6f37da032
export KERNEL_ADDRESS=0x51Af4C11f3b825E78F672065D80e2056E05bB305
export KERNEL_FACTORY_ADDRESS=0x798749411f57927230fFa2Cce094B451274E04D6
export ECDSA_VALIDATOR_ADDRESS=0x17B1B20Eb874d03f3221Cc4E40295cD5a7362c6B
export CALLER_HOOK_ADDRESS=0xE8C165907Ee014ebdD8eFFF70dad66f99165e9E2
export ECDSA_SIGNER_ADDRESS=0xCB52935BB59c23212fa9fBCAa9C55783Da6586Fc
# Smart account stack deployment at block 240503 timestamp 1779359929
export BUNDLE_MODULE_PLUGIN_ADDRESS=0xfCEf88AdF45644C6eDB7cE44E9d091a47cdD0Bd3
export WITHDRAWAL_LIMIT_POLICY_ADDRESS=0x9020b3C3033d1c1201b8e881C09C96Fe93460492
export BUNDLE_SPEND_PROTECTOR_HOOK_ADDRESS=0xC3Fdf1E3DE6767eeEa95028A8BF93817CA270BDF
export BUNDLE_WITHDRAW_HOOK_ADDRESS=0xdF25D88FED0FF8af2003Eb98E0CC153303fcAF2c
export SMART_ACCOUNT_FACTORY_ADDRESS=0xe28db1d1a138B21f2c84D7156b4Dab45a2F18E30

# The vault router is a precompile at a fixed address, NOT a deployment. It is also
# passed as the smart account's allowed `topUp` caller, and CallerHook reverts
# InvalidCaller for anything else - so a deployed-looking address here makes step 3's
# disbursement revert inside the sub-call.
export VAULT_ROUTER_ADDRESS=0x0000000000000000000000000000000000001017
```

## Running

All scripts accept `[envName]` as an optional last positional argument. Each prints
state before / after and a `CHANGES` summary.

```bash
# Show current state
npx tsx src/0-info.ts                                   # default env: local-reth
npx tsx src/0-info.ts outbe-peira

# Setup
npx tsx src/0-setup-native.ts
npx tsx src/0-setup-erc20.ts
# Bootstrap confidential Gratis for the user. Gratis AND Promis are both
# TEE-encrypted at rest, so neither can be plaintext-seeded at genesis - instead
# genesis seeds the user a Settled *gem* (scripts/seed-testnet.json "gems"). This
# script burns it for confidential Promis (IGemFactory.minePromis), then
# converts that Promis 1:1 into confidential Gratis (IGratisFactory.mineFromPromis).
npx tsx src/0-setup-gratis.ts                          # converts the whole gem load by default

# User pledges for a stablecoin credit line; the gratis it costs is derived on-chain
npx tsx src/1-pledge-gratis.ts                          # default: 1 stablecoin unit
npx tsx src/1-pledge-gratis.ts 1000 outbe-peira         # $1,000 of credit

# Deploy smart account (if needed) and fund with 1,000 USD
npx tsx src/2-top-up-smart-account.ts

# CCA requests credis against a prior pledge (latest ticket, or an explicit path).
# The disbursed amount and the asset come from the ticket, not from calldata.
# Payable: the CCA converts the protocol-6 collateral to the same whole-token
# amount of native-18 COEN and attaches it; that native value is escrowed
# against the position, returned when it settles in full, and burned if it voids.
# Requires the smart account from the previous step to already be deployed.
npx tsx src/3-request-credis.ts
npx tsx src/3-request-credis.ts tickets/pledge-abc123def456.json

# CCA spends from the bundle (within the daily limit policy)
npx tsx src/4-cca-simulate-purchase.ts

# Optional: user withdraws their free balance
npx tsx src/4.1-user-sa-withdraw.ts 5.5

# User settles a credis position. Defaults to the full payoff (accrued interest +
# outstanding principal); pass an amount to settle partially. Each payment releases
# the proportional collateral share back to the pledger's encrypted balance.
npx tsx src/5-user-settles.ts <positionId> [amount]
```

### Prerequisites this demo does not set up itself

- **A reserve vault registered for the ERC20.** `0-setup-erc20.ts` reads
  `assetVaultAt(erc20, 0)`; the vault itself is registered out-of-band via `addVault`,
  which nothing in this repo calls. Without it the setup step reverts.
- **The smart account must hold at least the loan amount** in its own ERC20 balance
  before step 3: the disbursement path tops the bundle up against that balance. Step 2
  moves 1,000 USD, so pledges above that fail inside the sub-call.
