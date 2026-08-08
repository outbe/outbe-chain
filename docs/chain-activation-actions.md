# Actions required to make the chain work

A chain that produces blocks is not a chain that works. Block production only
needs the founding committee; every economic path on top of it — Gem, Tribute,
Metadosis, Intex, Credis — is gated on state that somebody must put there, and
each gate fails with its own typed error until they do.

This document enumerates those actions. It answers "what must happen, by whom,
before which user action stops reverting".

## How to read this

Every action is tagged with when it can be performed:

| Tag | Meaning |
|---|---|
| **G** | **Genesis-fixed.** Must be in the genesis bundle. There is no runtime entrypoint that can add it to a running chain. Getting this wrong means regenerating genesis. |
| **B** | **Bootstrap.** A one-time action on a running chain, before the dependent path is usable. |
| **R** | **Recurring.** Must keep happening. If it stops, the dependent path degrades or halts. |

Each entry names the authority that may perform it, the concrete mechanism, and
the typed failure the chain produces while the action is outstanding.

---

## Layer 0 — Blocks

This layer is satisfied entirely by the genesis bundle. Nothing here is an
operator action on a live chain.

### G-0.1 Founding committee of exactly four validators

- **Authority:** genesis author.
- **Mechanism:** `scripts/prepare_network.py` (`--generate-validators 4`).
  `scripts/bootstrap-testnet.sh` rejects any other count outright.
- **Seeds:** ValidatorSet records, `staking.genesis_validator_stake`,
  `staking.min_stake`, `validator_set.max_validators`, public DKG artifacts
  (`polynomial.hex`, `dkg-output.hex`).
- **Without it:** no committee, no blocks.

OCOMP V1 fixes the founding committee at four members. This is a protocol
constant, not a deployment preference.

### G-0.2 `teeAttestationV1` in the ChainSpec

- **Authority:** genesis author.
- **Mechanism:** `seed_tee_policy()` in `scripts/seed_genesis.py` writes the
  canonical `TeePolicyV1` and its policy hash.
- **Without it:** every node role refuses to start. There is no tee-less chain,
  no in-process stub, and no offer-free exception — see
  [`becoming-a-validator.md`](becoming-a-validator.md).

### G-0.3 OCOMP fork install in genesis `extra`

- **Authority:** genesis author.
- **Mechanism:** `OcompForkInstallV1` under the genesis `extra` key, decoded at
  node startup (`crates/blockchain/node/src/ocomp/fork.rs:38`). A fresh-devnet
  profile requires a hash-bound, genesis-active `Measurement@1` install and the
  exact `metadosisStorageLayoutV1.layoutHash` before Cycle block 1; both
  bindings are validated before the process launches.
- **What it installs** (`crates/core/metadosis/src/commands.rs:113`,
  `install_fork_profile`): founder OCOMP registrations in ValidatorSet, the
  Metadosis fork install, and the fresh OCOMP profiles for Tribute and Oracle.
- **Without it:** `submitLysisResult` reverts with `OcompResultVoteRejected(5)`
  and no day can be processed through the verified OCOMP path. There is no
  synchronous Lysis fallback for populated positive-gratis days.

---

## Layer 1 — Node admission (TEE)

### B-1.1 TeeRegistry bootstrap

- **Authority:** the founding committee, automatically.
- **Mechanism:** the block-1 bootstrap registration writes the one-time
  `TeeBootstrapData` (`TeeRegistry::write_bootstrap`), installing the canonical
  offer public key and the per-validator `keysHash(addr)` bundles. A second
  bootstrap is rejected as defense in depth.
- **Observation:** `cast call 0x…EE0A 'isBootstrapped()(bool)'` — this is
  exactly what `scripts/tribute-demo-actions.sh` waits on before offering.
- **Without it:** the Tribute offer public key is zero; clients have nothing to
  encrypt to.

### B-1.2 Per-node `tee join`

- **Authority:** each node operator, per node.
- **Mechanism:**
  ```sh
  outbe-cli tee join --enclave-socket 127.0.0.1:7000 \
    --profile validator|full-node \
    --node-data-dir "$NODE_DATA_DIR" \
    --reth-p2p-secret-key "$RETH_P2P_SECRET" \
    --binding-id "$BINDING_ID" --valid-until "$VALID_UNTIL" \
    --private-key "$RELAY_EVM_KEY" --rpc-url http://<certified-rpc>:8545
  ```
- **Ordering:** must complete *before* the node process starts. Startup compares
  the upstream canonical 32-byte offer key against the resident key and refuses
  to launch Reth on mismatch.
- **Without it:** the node cannot join, sync as a certified follower, or execute
  canonical transactions.

### R-1.3 Enclave lease renewal

- **Authority:** each node operator.
- **Mechanism:** `outbe-cli tee renew-now`.
- **Without it:** the binding lapses at its committed expiry and the node drops
  out of admission. Old-policy leases stay usable until expiry, but after an
  enclave-policy activation the old policy can neither renew nor create a new
  binding.

---

## Layer 2 — Price truth (Oracle)

**This layer is where most "why does everything revert" answers live, and the
registry half of it is genesis-only.**

### G-2.1 Trading pairs

- **Authority:** genesis author only.
- **Mechanism:** `oracle.pairs` in the seed file →
  `init_from_genesis` → `register_pair` (`crates/system/oracle/src/logic.rs:182`).
- **Why it is genesis-only:** `register_pair` has no precompile entrypoint. The
  runtime surface exposes only `activateVoteTarget` / `deactivateVoteTarget`,
  both of which require `caller == Address::ZERO` (system) *and* an
  already-registered pair (`logic.rs:765-799`). A pair that is not in genesis
  cannot be added by any transaction, vote, or operator.
- **Without it:** every downstream price read fails, and there is no recovery
  short of a new genesis.

### G-2.2 Settlement currencies (ISO → denom → pair)

- **Authority:** genesis author only.
- **Mechanism:** `oracle.settlement_currencies` in the seed file. Each entry is
  `(iso_code, denom, pair_base, pair_quote)` and is validated at import: nonzero
  ISO code, non-empty denom, the pair must already be registered, and the ISO
  code must not already be mapped (`logic.rs:200-237`).
- **Also genesis-only:** reference currencies and S-curve seeds
  (`oracle.scurve_seeds`).
- **Without it, by caller:**
  - `GemFactory` → `IssuanceCurrencyNotRegistered { iso_code }`
    (`crates/core/gemfactory/src/runtime.rs:409`)
  - `TributeFactory` → `IssuanceCurrencyNotRegistered { issuance_currency }`,
    or `SettlementCurrencyPairNotRegistered` if the ISO maps to a pair that has
    no id (`crates/core/tributefactory/src/runtime.rs:263,269`)

### B-2.3 Feeder delegation

- **Authority:** each validator, for its own operator key.
- **Mechanism:** `outbe-cli validator delegate --role oracle --to <feeder>`, or
  `IOracle.delegateFeederConsent`. Genesis can pre-seed these via
  `oracle.feeder_delegations`.
- **Why:** it gives the price feeder a role-scoped key so a hot feeder process
  never holds general validator authority (PFS-011).
- **Without it:** the validator must sign price votes with its validator key, or
  it does not vote at all.

### R-2.4 Price submission every vote period

- **Authority:** validators or their delegated feeders.
- **Mechanism:** `outbe-feeder` submitting `IOracle.submitVote(tuples)`; the
  tally runs in the Oracle pre-execution hook at each vote-period boundary
  (`crates/system/oracle/src/hooks.rs:51`).
- **Without it:** exchange rates stay at their genesis values and then go stale.
  `GemFactory` reverts with `OracleUnavailable` once a rate reads zero; VWAP for
  the current worldwide day is absent, so Tribute pricing falls back to the
  S-curve alone. With `penalties_enabled`, persistent non-submission drives the
  slash-window path and force-exits underperformers.

**This is the single most important recurring action on the chain.** Everything
priced — Gem cost and floor, Tribute nominal price, Metadosis day limit —
degrades the moment it stops.

---

## Layer 3 — Settlement assets (Stablecoin + Vault)

This is the layer the question's example points at, and the dependency is real:
**at least one stablecoin must be registered, and it must be given a vault,
before any value can settle.**

### B-3.1 Register at least one stablecoin

- **Authority:** any account that can post the bond; approval is by validator
  vote.
- **Mechanism:** a bonded `StablecoinCreate` proposal through Vote. The Factory's
  public EVM surface is read-only — Vote is the only creation adapter.
  ```sh
  outbe-cli --private-key "$ISSUER_KEY" stablecoin propose \
    --name "..." --ticker USDX --iso4217 840 --decimals 6 \
    --supply-cap <units> --policy-id <existing policy id>
  ```
  Then validators cast votes; quorum is `yes * 3 >= active_validators * 2`.
- **Constraints:**
  - Bond is `STABLECOIN_CREATE_BOND` = 1,000,000 × 10¹⁸
    (`crates/blockchain/primitives/src/stablecoin_fork.rs:23`).
  - The proposer must equal the payload `issuer`
    (`crates/blockchain/evm/src/handlers.rs`).
  - The payload must be byte-exact canonical JSON — `decode_canonical_stablecoin_create`
    re-encodes and compares, rejecting anything else as `NonCanonicalEncoding`.
  - `policy_id` must already exist. Use the built-in `ALLOW_ALL_POLICY_ID` (`1`)
    or create one first with `outbe-cli stablecoin policy-create`. `DENY_ALL`
    is `0`.
  - At most 16 pending public bonded proposals chain-wide, one per proposer.
- **Approval effect:** initializes one zero-supply ledger, installs the exact
  native marker, commits the registry, emits `StablecoinCreated`. Expiry
  releases the reservation.
- **Scope caveat, verbatim from the module:** Factory registration means
  protocol admission only. It is not evidence of backing, redeemability, price
  stability, issuer solvency, creditworthiness, fee-asset eligibility, or
  payment-lane eligibility.

### B-3.2 Register a vault for that stablecoin

- **Authority:** the VaultRouter `owner`, seeded at genesis
  (`vault_router.owner` in the seed file).
- **Mechanism:** `IVaultRouter.addVault(address vault)` at
  `0x…1017`. The vault's asset must be the stablecoin, and the token must report
  an `isoCode()` matching the settlement currency.
- **Without it:** `first_vault()` reverts with `ReserveVaultNotConfigured`
  (`crates/core/vaultrouter/src/runtime.rs:427`). Concretely:
  - `GemFactory.settleGem` cannot deposit `cost_amount` into the reserve vault
    (`gemfactory/src/runtime.rs:332` → `vaultrouter::api::deposit`).
  - `IntexFactory` rejects the token with `PaymentTokenNotRegistered(token)`,
    because acceptance is `assetVaultsCount(token) > 0` *first*, ISO check second
    (`crates/core/intexfactory/src/runtime.rs:558`).

### B-3.3 Register liquidity sources and targets

- **Authority:** VaultRouter `owner`.
- **Mechanism:** `addLiquiditySource(address, StablesSource)` and
  `addLiquidityTarget(address, StablesTarget)`.
- **Without it:** `deposit` reverts with `InvalidLiquiditySource` for any caller
  whose registered source type resolves to `Unknown`
  (`vaultrouter/src/runtime.rs:302-334`). GemFactory settlement is one such
  caller.

**The three together are one unit.** A registered stablecoin with no vault, or a
vault with no registered source, moves no value. B-3.1 → B-3.2 → B-3.3 must all
land before the first `settleGem` succeeds.

---

## Layer 4 — Daily value cycle

### Automatic: worldwide-day advancement

No operator action. Two daily begin-zone Cycle ticks drive
`FORMING → LOOKBACK_DELAY → OFFERING → WAITING → READY`:

- **00:00 UTC** — `emission_limit_1`: creates the next day, settles READY days.
- **12:00 UTC** — `wwd_advance_noon`: status advancement only. This tick exists
  because the forming/offering window edges land at 12:00 UTC; without it every
  offering window opened ~12 hours late.

READY work is ordered by `(scheduled_process_time, worldwide_day)`, one day per
tick.

### G-4.1 Seed the first worldwide day (optional but usual)

- **Authority:** genesis author.
- **Mechanism:** `metadosis.worldwide_days` in the seed file — `wwd`, `status`,
  `day_type`, window timestamps, `current_vwap`, `day_limit`.
- **Alternative:** the fresh-devnet profile deliberately starts *without* a
  pre-seeded active day and observes runtime `Create` in finalized block 1.
- **Note:** `current_vwap` must be nonzero — `VwapMustBeNonZero`.

### R-4.2 Validator OCOMP readiness

- **Authority:** each validator.
- **Mechanism:** `outbe-cli validator confirm-ready`, plus
  `outbe-cli validator delegate --role ocomp --to <key>` for a role-scoped
  submission key.
- **Semantics:** OCOMP voting membership is not configured separately. Each
  attempt pins the ordered ACTIVE ValidatorSet snapshot that exists when the job
  is committed, derives `N` from it, and derives quorum with
  `simplex_n3f1_quorum(N)`. A validator joins *future* jobs only after
  `confirmValidatorReady` is accepted and the certified DKG/reshare boundary
  makes it ACTIVE; already-open jobs keep their historical snapshot.
- **Without enough ready validators:** jobs cannot reach quorum, days exhaust
  their per-day `max_terminal_job_records` budget (365) and fail with
  `AttemptsExhausted`, routing the retained Lysis budget to Promis carry-over.
  This is a day-level terminal outcome, never a chain-level halt.

### R-4.3 Run the OCOMP off-chain services

- **Authority:** each node operator.
- **Mechanism:** the SnapshotExporter/worker services and `outbe-ocomp follower`,
  sharing `OCOMP_CHAIN_ID`, `OCOMP_GENESIS_HASH`, `OCOMP_BOOT_NONCE`,
  `OCOMP_PROTOCOL_BUNDLE_HASH`, `OUTBE_OCOMP_BASE_PATH`, `OUTBE_OCOMP_NODE_USER`.
  The six `--ocomp.*` node arguments are all-or-nothing per profile.
- **Without it:** no local Lysis result to vote on.

---

## Layer 5 — Validator lifecycle (steady state)

### R-5.1 Join

Three steps, in order:

1. `outbe-cli validator register` — BLS proof-of-key, no stake. → `REGISTERED`
2. `outbe-cli staking stake` at least `config_min_stake`. → `PENDING`
3. Wait for the next reshare boundary. → `ACTIVE`

Consensus signers are validators that still hold a BLS share: `ACTIVE`,
`EXITING`, and temporarily `JAILED` until the next reshare clears the share.
Non-voting followers may be `REGISTERED`, `PENDING`, or `JAILED`.

Genesis validators skip 1–2; `seed_validator_set` and `seed_staking` write them
in directly.

### R-5.2 Stay in

- Vote in consensus — daily emission is delivered as gems (`Genesis` gems for the
  first 21 days, `Validator` gems after) distributed proportionally to voting
  participation. There is no claimable native `pending_rewards` balance.
- Submit Oracle prices (R-2.4) — non-submission is penalized through the
  slash-window path.
- `outbe-cli staking unjail` after a downtime jail.
- Keep the enclave lease alive (R-1.3).

---

## Layer 6 — Governance and upgrades

### R-6.1 Protocol version activation

- **Authority:** validators, by vote.
- **Mechanism:**
  ```sh
  outbe-cli --private-key "$VALIDATOR_KEY" vote propose \
    --target-module 0x…EE0B \
    --payload '{"version":"1.2","activationHeight":12345,"info":"v1.2 rollout"}'
  outbe-cli --private-key "$VALIDATOR_KEY" vote cast --proposal-id 1 --yes
  ```
- **Rules:** version strictly greater than `active_version`; `activationHeight`
  at least `MIN_ACTIVATION_BUFFER` blocks out (0 on the localnet chain id); at
  most one scheduled update per activation height.
- **Operator obligation:** roll binaries out *before* the activation height. A
  node whose binary protocol version is older than `active_version` refuses to
  start.

### R-6.2 Enclave policy rollout

The same Update vote is the **only** authority for a successor enclave policy —
there is no separate attestation-policy proposal. Add `teePolicy` (canonical
`TeePolicyV1` as bounded lowercase hex) to the update payload. Approval requires
the exact current-policy predecessor hash, version `current + 1`, and an empty
`next` slot. Between approval and activation, bindings migrate with
`transitionEnclaveMeasurement`; at the activation height, `next` is promoted to
`current` in the same checkpoint as the protocol version.

### R-6.3 Canon and meta-canon

The three registered vote targets are Update (`0x…EE0B`), Governance
(`0x…1018`), and StablecoinFactory (`0x…EE0F`)
(`crates/blockchain/evm/src/handlers.rs:26-32`). Governance owns canon /
meta-canon overwrite and the OIP/GIP proposal lifecycle; genesis seeds the
initial canon via `seed_governance`.

---

## Layer 7 — Feature-gated, only if used

| Action | Tag | Authority | Mechanism | Failure without |
|---|---|---|---|---|
| Register an L2 network | B | permissionless | `IL2Registry.registerNetwork(chainId, l1Address, blsPublicKey)` at `0x…EE0E` | `NetworkNotRegistered { chain_id }` |
| Configure a remote VaultRouter | B | VaultRouter owner | `set_remote_vault_router` | `RemoteVaultRouterNotConfigured(chainId)` |
| Configure crosschain bridge / asset / token bridge | B | VaultRouter owner | crosschain config setters | `CrosschainBridgeNotConfigured`, `CrosschainAssetNotConfigured`, `CrosschainTokenBridgeNotConfigured` |
| Deploy external contracts | G | genesis author | `seed_external_contracts` — CREATE2 deployer, Permit2, The Compact, EntryPoint v0.7, SenderCreator v0.7 | account-abstraction and permit/allocation paths missing |

ZeroFee needs **no** registration action. The sponsored-target whitelist is a
protocol constant (`SPONSORED_TARGET_WHITELIST`), and admission is decided per
transaction by shape — zero value, zero priority fee, whitelisted target,
bounded gas and calldata — plus a signer-balance anti-sybil check. Genesis only
writes the slot-0 schema version.

---

## Minimum viable action set

For a chain that can take a Tribute offer and settle a Gem, in order:

**In genesis (no recovery if omitted):**

1. Four founding validators with stake and DKG artifacts — G-0.1
2. `teeAttestationV1` policy — G-0.2
3. OCOMP fork install bound to `Measurement@1` and the Metadosis layout hash — G-0.3
4. Oracle pairs — G-2.1
5. Oracle settlement currencies mapping ISO → denom → pair — G-2.2
6. VaultRouter owner — prerequisite for B-3.2 and B-3.3

**On the running chain:**

7. TeeRegistry bootstraps at block 1; wait for `isBootstrapped()` — B-1.1
8. Every node completes `tee join` before starting — B-1.2
9. Validators delegate feeder keys and feeders start submitting prices — B-2.3, R-2.4
10. **Register at least one stablecoin** by bonded vote — B-3.1
11. Add a vault for it — B-3.2
12. Register GemFactory as a liquidity source — B-3.3
13. Validators `confirm-ready` for OCOMP and run the follower services — R-4.2, R-4.3

Steps 10–12 are the ones most easily missed, because the chain looks entirely
healthy without them — blocks finalize, RPC answers, validators earn — right up
until the first `settleGem` reverts with `ReserveVaultNotConfigured`.

---

## Verification

```sh
RPC=http://127.0.0.1:8545

# L1 — TEE registry bootstrapped
cast call 0x000000000000000000000000000000000000EE0A 'isBootstrapped()(bool)' --rpc-url $RPC

# L2 — pairs registered and priced
outbe-cli --rpc-url $RPC oracle pairs
outbe-cli --rpc-url $RPC oracle rates
cast call 0x000000000000000000000000000000000000EE05 \
  'getSettlementCurrency(uint16)(bytes32,bytes32)' 840 --rpc-url $RPC

# L3 — at least one stablecoin, and it has a vault
cast call 0x000000000000000000000000000000000000EE0F 'tokenCount()(uint256)' --rpc-url $RPC
cast call 0x0000000000000000000000000000000000001017 \
  'assetVaultsCount(address)(uint256)' $TOKEN --rpc-url $RPC
cast call 0x0000000000000000000000000000000000001017 \
  'liquiditySourcesCount()(uint256)' --rpc-url $RPC

# L4/L5 — committee and day state
outbe-cli --rpc-url $RPC validator list
outbe-cli --rpc-url $RPC validator participation

# L6 — active protocol version
cast call 0x000000000000000000000000000000000000EE0B 'getActiveVersion()(uint32)' --rpc-url $RPC
```

`tokenCount() == 0` is the direct read for "no stablecoin is registered yet".

## Related documents

- [`becoming-a-validator.md`](becoming-a-validator.md) — node roles, lifecycle,
  `tee join`, full operator flags.
- [`dcap-testnet-launch.md`](dcap-testnet-launch.md) — the exact four-founder
  DCAP testnet checklist.
- [`flows/index.md`](flows/index.md) — cross-module protocol flow specifications;
  PFS-006 (validator join), PFS-010 (stablecoin lifecycle) and PFS-011
  (operational key delegation) cover the actions above end to end.
- [`verify-tribute-localnet.md`](verify-tribute-localnet.md) — smallest
  end-to-end check that the stack works.
