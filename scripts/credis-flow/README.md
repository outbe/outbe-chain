# Credis User-Flow Demo (`scripts/credis-flow`)

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
  `pledgeNote`; `unpledgeGratis(amountStables, pledgeNote, mac, opNonce)`;
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
- **Credis.** The CCA first calls `IVaultRouter.reserveStables(smartAccount, asset, amount)`
  to lock vault liquidity for 15 minutes. The user then pledges Gratis. After the
  user hands over the spend path, the CCA calls
  `issueCredis(smartAccount, pledgeNote, spendAuth, referenceCurrency, reservationId)`
  (payable: native COEN equals pledged GRATIS after the existing 6-to-18 decimal conversion).
  The user shares the note, designated smart account and recipient-bound `spendAuth`.
  The account receives COEN; the issuing CCA receives the sealed stablecoin principal
  **to cover COEN delivered to the user's smart account**. Account stablecoins stay unchanged.
  Neither the asset nor principal is caller-selected at issuance; both remain sealed in the note.
  The client requires existing account stablecoins covering principal and verifies the live
  CCA signer, token policy, cap, selector access, owner delay and reservation before issuance.
  These checks are off-chain. Excess reservation funds return to the originating vault.
  The originating CCA can return a reservation anytime; anyone can return it strictly after expiry.
  `settle(positionId, amount)` applies a payment interest first and principal
  second, and **automatically** releases the collateral share proportional to the
  principal it covered back to the pledger's encrypted balance - no reclaim note,
  no separate unpledge step.

Crypto uses Node's built-in `crypto` (HKDF-SHA256, HMAC-SHA256, ChaCha20-Poly1305)
plus `@noble/curves` for X25519. `npm run generate-types` stages the ABIs and runs
typechain; `npx tsc --noEmit` is clean.


CCA registration requires a separate **1,000,000,000 COEN** self-bond. The
`setup-native` step funds the remaining bond and calls `ICcaRegistry.bond("Credis Flow CCA")` using
`CCA_PRIVATE_KEY`; its funding wallet must hold that amount plus operating funds.
Partial bonds accumulate, but smart-account creation and origination require the
full bond. Pending unbonds must be claimed before registration can resume.

### First-run quickstart

```bash
cd scripts/credis-flow
npm install
npm run generate-types
cp .local-reth.env.example .local-reth.env   # then fill values
npm run info                                  # read-only state snapshot
```

`npm run generate-types` reads chain precompile ABIs from this checkout and the
five smart-account ABIs from a sibling `smart-account` checkout, then emits
`src/contracts/`. Clone `outbe/smart-account` alongside this chain repository, or
set its location explicitly (relative paths resolve from the command's working directory):

```bash
SMART_ACCOUNT_REPO=/path/to/smart-account npm run generate-types
```

The imported ABIs are `SmartAccountFactory` (account prediction/creation and module
addresses), `ExecutionDelayPolicy` (owner scheduling, readiness, and events),
`WithdrawalLimitPolicy` (token restrictions and caps), `IEntryPoint` (nonces,
deposits, and user operations), and `IERC20` (balances, transfers, and approvals).
Regenerate them with `mise run export-abi` in the smart-account repository when
its interfaces change. Deployment addresses still come from the environment files below.

## Configuration

Each script reads two env files from the project root, selected by the `envName`
CLI argument (default: `local-reth`):

- `.${envName}.env` - RPC URL, private keys, fixed addresses
- `.${envName}.deployment.env` - addresses produced by the Foundry deploy scripts

## Running

All scripts accept `[envName]` as an optional last positional argument. Each prints
state before / after and a `CHANGES` summary.
See all available scripts and their order in the `package.json`.


## Fresh deployment workflow

Existing accounts are not migrated. Redeploy the smart-account stack and use its new
factory address; the simplified factory changes predicted account addresses.
Kernel v4 and its pinned dependencies remain unchanged.

```bash
npm run setup-native -- local-reth
npm run setup-erc20 -- local-reth
npm run setup-gratis -- local-reth
npm run setup-account -- local-reth     # deploy and fund through ordinary ERC20 transfer
npm run reserve-stables -- 1 local-reth
npm run pledge-gratis -- 1 local-reth
npm run request-credis -- local-reth
npm run cca-simulate-purchase -- 1 local-reth
npm run user-sa-withdraw -- 1 local-reth # schedules; repeat after 300 seconds to execute
npm run user-settles -- <positionId> <fixed-amount-in-minor-units> local-reth
# Repeat the identical settlement command after 300 seconds.
```

CCA withdrawals are immediate canonical single-token transfers, capped at `1000e6`
minor units per token per day. Validation debits the cap even when execution fails;
issuance's direct router payout does not use this cap.

Owner transfers, approvals, batches and security changes require scheduling and a
300-second delay. Only standalone zero-value scheduling/cancellation calls are exempt.
Requests bind chain, account, installation generation and exact execution calldata;
they never expire automatically. Cancellation followed by rescheduling starts a new delay.
Execution consumes a request, while a reverted execution restores it. ERC-1271 and
signature-based module enablement are disabled. ROOT-mode operations are rejected
because pinned Kernel v4 deliberately bypasses hooks for ROOT; use the owner permission.
A delayed root/policy replacement permits departure from this configuration.

`Scheduled`, `Cancelled` and `Executed` events support CCA monitoring. The issuance
client stops for changed configuration or pending owner calls it cannot establish are
ordinary configured-token transfers. This conservatively includes arbitrary calls and batches.
Owner commands save exact requests and transaction hashes under gitignored `tickets/`;
reruns recover receipts, check readiness and retry failed execution. EntryPoint deposits
remain separate from account funding. Never share view/modify keys or the full local
ticket: share only `pledgeNote`, `smartAccount` and `spendAuth` with the CCA.
