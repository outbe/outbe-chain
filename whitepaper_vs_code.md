# Whitepaper vs Code — Deviation Report

**Baseline**: `whitepaper.md` v1.0 (March 2026) vs repository state on branch `feat/up-mcp` (July 2026).
**Method**: every load-bearing whitepaper claim was verified against the code. Code is the source of truth; `README.md` was used as a secondary cross-check.
**Verdicts**: `MATCH` — code does what the whitepaper says; `DIVERGED` — code does something different; `PARTIAL` — mechanism exists but details differ; `NOT IMPLEMENTED` — whitepaper describes design that has no runtime counterpart.

This file records the delta. `whitepaper.md` has been rewritten to match the code; this report is the audit trail of what changed and why.

---

## 1. Top-level inversions (claims that were architecturally wrong)

| # | Whitepaper v1.0 said | Code actually does | Evidence |
|---|---|---|---|
| 1 | Finalized-parent certificate rides as a reserved **end-of-block** system tx at `0xEE04` | It is the **first begin-zone** system tx, `SystemTxKind::CertifiedParentAccounting` (selector `OSA3`, payload `CertifiedParentAccountingMetadata`, magic `OAV3`), a **direct-parent** proof to `OUTBE_SYSTEM_TX_ADDRESS = 0xff00…0001`. `0xEE04` is `ACCOUNTING_PROGRESS_ADDRESS` — a dispatch-less marker account (slot 0 = `last_accounted_block_number`, `0xef` bytecode) | `crates/blockchain/primitives/src/system_tx.rs:60,105,167`, `consensus_metadata.rs:77-109`, `addresses.rs:104-114`, `crates/system/accounting/src/runtime.rs:28-45` |
| 2 | Emission is allocated **per block** by an `EmissionLimit` begin-block hook; sinks = Validator 4% / AgentReward 8% / CCA 4% / Metadosis ~84% | Emission is **per day**, dispatched by the `Cycle` module's 00:00 UTC `CycleTick`; sinks = Validator 4% / WAA 4% / SRA 4% / CCA 4% / Merchant 4% / Metadosis terminal 80%. EmissionLimit no longer runs in begin_block at all | `crates/system/emissionlimit/src/allocation.rs:14-81`, `crates/system/cycle/src/handler.rs:90-103`, `crates/blockchain/evm/src/executor.rs:431-435` |
| 3 | Validator emission is recorded as claimable `pendingRewards`; fee shares paid immediately to voters | Validator emission is paid as **Gems** (`Genesis` gems for 21 days from genesis, `Validator` gems after), proportional to participation. There is **no claimable pending-rewards balance and no `claimReward`** on the Rewards precompile (read-only). Fees are escrowed and settled at **N+K (K=3)** over the late-finalize inclusion window with decay `[100,100,100,0]` and a fixed denominator; absent voters' shares are **burned**, not redistributed | `crates/system/rewards/src/api.rs:89-142`, `precompile.rs:15-25`, `constants.rs:27-125`, `finalized_metadata_hook.rs:88-180` |
| 4 | Ethereum-style slashing: initial 1/32 burn, correlation penalty over ~36-day window, inactivity leak (2²⁴ quotient, bias 4), churn-limited exit queue (min 4 / quotient 65536), `FaultSource` registry | None of that is implemented. Actual model: slash = **5%** (`DEFAULT_SLASH_AMOUNT_PERCENT = 5`) burned via `decrease_balance`, felony → **JAILED** with an `unjailValidator` recovery path (cooldown + stake ≥ min) — not forced-exit-only. No correlation penalty, no inactivity leak, no churn queue, no FaultSource registry | `crates/system/slashindicator/src/runtime.rs:43-53`, `crates/system/staking/src/logic.rs:380-381`, `crates/system/validatorset/src/runtime.rs:889-933`; repo-wide grep: no churn/correlation/inactivity constants |
| 5 | Slashing auto-fires for proposer-offline, voter-offline, oracle non-participation, equivocation | Only **voter-offline** (window-close absentees) and **oracle non-participation** (in the Oracle module, not SlashIndicator) auto-fire. Proposer-offline is **dormant**: the V2 verifier forces `missed_proposers` to be empty. Equivocation/VRF evidence paths are **manual precompiles** (no in-node watcher). `slash_byzantine` is dead code | `crates/blockchain/consensus/src/proof/verifier.rs:316-320`, `begin_block_precompile.rs:798-865`, `crates/system/oracle/src/tally.rs:511-543`, `slashindicator/src/runtime.rs:407-1333` |
| 6 | Business daily flow: Green Day → Lysis, Red Day → **Touch**; gratis rate 8% of tribute amount, per-tribute fraction 8–16% | **Touch does not exist.** Lysis runs on both day types; Red Day divides demand/supply by `RED_DAY_REDUCTION_COEF = 8` and skips auction clearing. Symbolic rate = **32%** (`SYMBOLIC_RATE`), per-tribute fraction default **32%**, ceiling **64%** (`F_FP_DEFAULT`, `F_MAX_FP`) | `crates/core/metadosis/src/runtime.rs:42-52,470,539-541`, `constants.rs:13-17`, `crates/core/lysis/src/constants.rs:11-13` |
| 7 | TributeFactory validates offers with **ZK proofs (PlonK)** | Offer validation is **TEE enclave attestation** (`verify_tribute_offer_attestation`, DCAP `QuotePolicy`). The chain's general ZK precompile is **Groth16**, not PlonK | `crates/core/tributefactory/src/enclave_offer.rs:25,150`, `crates/system/zkproof/src/precompile.rs:25` |
| 8 | DKG rotation cadence `dkgRotationIntervalBlocks = 21000`; pure VRF rotation completes **without Simplex restart** | The `dkgRotationIntervalBlocks` genesis key is **rejected as deprecated**; cadence is `epochLengthBlocks` (default **1200**). Every boundary activation **unconditionally restarts Simplex** (engine task abort + rebuild), including same-set VRF-only rotations — the no-restart branch does not exist | `crates/blockchain/engine/src/stack.rs:168-172,340-347,4176-4197,4417,4573`, `config.rs:88-100` |
| 9 | DKG completes on threshold participation in all cases | Live-chain reshares are quorum-based (2f+1 players, dealer-quorum logs), but the **genesis bootstrap DKG is n-of-n**: it requires all `n` dealer logs and fail-fast aborts if a genesis validator is unreachable | `crates/blockchain/consensus/src/dkg_actor/actor.rs:133-142,168-180,477,490` |
| 10 | §9 State migration: versioned runtime state, fork-activated migration functions, chunked migrations | **Not implemented.** No per-module schema-version machinery, no migration registry, no chunked migrations. Slot 0 is reserved by convention only. The only `migrate_*` in the tree is a filesystem move of DKG key files at startup | `crates/blockchain/engine/src/stack.rs:5184-5199`, `crates/system/zerofee/src/schema.rs:14-17` |

---

## 2. Consensus and crypto (§2–§3)

| Claim | Verdict | Actual behavior |
|---|---|---|
| Two Tokio runtimes, `thread::spawn`, in-process engine handle, no HTTP Engine API | MATCH | Reth on the main runtime; consensus on `commonware_runtime::tokio::Runner` in a spawned thread; `ConsensusEngineHandle` calls `new_payload` / `fork_choice_updated` directly (`bin/outbe-chain/src/main.rs:329-350,480`, `consensus/src/executor/actor.rs:469-692`) |
| `HybridSignature { vote_signature, seed_partial }` | DIVERGED | Real fields: `bls_individual_vote`, `vrf_material_version`, `bls_seed_partial`, **plus `seed_partial_identity_sig`** — an attribution binding that makes byzantine seed partials slashable (`consensus/src/hybrid.rs:80-96`) |
| `HybridCertificate { aggregated_vote, signer_bitmap: BitVec, vrf_proof: Option }` | PARTIAL | Real fields: `signers: Signers` (commonware `BitMap<1>`), `bls_aggregated_vote`, `vrf_proof: Option<VrfProof>`. Behavior matches: finality = aggregate + bitmap; `assemble` sets `vrf_proof = None` on recovery failure without failing finality (`proof/hybrid_wire.rs:74-81`, `hybrid.rs:776-882`) |
| `is_batchable() = true` | DIVERGED | Returns **`false`** (`hybrid.rs:888-890`) |
| Certificate ~162 B; 2 pairing checks; ~4 ms | PARTIAL | ~162 B exists only as a doc comment; the size test asserts `< 200` B at n=3. Finality is one `aggregate::verify_same_message` call (2 pairings); the 4 ms figure is not encoded anywhere |
| Leader = `hash(seed, view) % n` | PARTIAL | Leader = `modulo(seed_bytes, n)` — big-endian byte reduction of the raw threshold-signature seed, **no hash and no view mixing** on the live path (view is appended only in multi-view recompute) (`hybrid/election.rs:103-184`) |
| Genesis view 1 round-robin `(epoch + view) % n` | MATCH | Exact formula (`election.rs:172-174`); an intermediate deterministic `bootstrap_seed \|\| round` fallback also exists |
| VRF grace window, degraded deterministic selection, fail-closed after expiry | MATCH | `VrfSafetyGate` with `Healthy/Degraded/Expired`; expiry = `planned_activation + grace`; `ensure_block_allowed` errors past expiry (`vrf_safety.rs:146-181`) |
| `mixHash = SHA-256(vrf_proof)` | MATCH (detail) | SHA-256 over the **threshold-signature bytes** (`proof.threshold_signature.encode()`), not the whole proof struct; retains prior seed + marks degraded when absent (`reporter.rs:426-429`, `finalization/actor.rs:437-441`) |
| Max validators 128 | DIVERGED | Protocol codec cap is `MAX_VALIDATORS = 256`; 128 is a design-reference comment only (`consensus/src/bls.rs:29-30`, `config.rs:41`) |
| `activateResharedSet()` as a system tx | PARTIAL | It is a ValidatorSet call made by the **executor** while applying the finalized `BoundaryOutcome` header artifact on the first new-epoch block — not a standalone system tx (`primitives/src/consensus.rs:149-183`, `evm/src/executor.rs:3182-3267`) |
| DKG ~3–10 s healthy | UNVERIFIABLE | Only `DKG_TIMEOUT = 120 s`, `RETRY_INTERVAL = 5 s` exist (`dkg_actor/actor.rs:98-102`) |
| Stale shares: detect mismatch, stop, wait | MATCH (nuance) | Validated against `boundary.vrf_group_public_key` (keccak of polynomial public); mismatch is mostly **fail-fast** or routes to the startup live-join reshare — not a silent "keep running and wait" (`stack.rs:350-363,5893-6030`) |

## 3. Consensus data flow and block lifecycle (§3.5, §6)

| Claim | Verdict | Actual behavior |
|---|---|---|
| `header.extra_data` carries BoundaryOutcome + DealerLog, 64 KiB cap | MATCH (incomplete) | `OutbeBlockArtifacts` (magic `OART`) tags: `0x01` execution_summary, `0x02` BoundaryOutcome, `0x03` DealerLog, `0x04` **retired/rejected**, `0x05` timestamp_millis_part, `0x06` late_finalize_credits, `0x07` committee_preannounce; `OUTBE_MAX_EXTRA_DATA_SIZE = 64 KiB` (`primitives/src/reshare_artifact.rs:44-52`, `consensus.rs:26`). Whitepaper omitted tags 0x01/0x05/0x06/0x07 |
| Bridge queues finalized-parent artifacts | DIVERGED | `ConsensusExecutionBridge` holds only `genesis_validators`, `consensus_status`, `execution_summary_cache`, `pending_tee_bootstrap`, `finalization_fetcher`. Finalized-parent proofs moved to the parent-cert store + Phase 1 system tx. The "not a source of truth" part matches (`primitives/src/consensus.rs:354-444`) |
| §6.1 timing model (EmissionLimit first; slashProposer; recordProposer; recordParticipation; fees; end_block) | DIVERGED | Real order — storage hooks: genesis validation → Rewards begin → validator-set epoch boundary → `Staking::process_unbonding` → Oracle → Nod → Gem → Intex. Receipt-visible begin-zone phases (ord 0–5): `CertifiedParentAccounting` → `LateFinalizeCredits` → `CycleTick` → `BoundaryOutcome` (opt) → `TeeBootstrap` (opt, one-time) → `OracleSlashWindow` → user txs. `recordProposer` lives inside CycleTick; participation/absentee accounting is deferred to window-close at N+K; **no module implements `end_block`** (`evm/src/executor.rs:416-492,2909-2952`, `primitives/src/system_tx.rs:10-19,165-174`) |
| System tx: `gas_price = 0`, `sender = block.coinbase` (proposer) | PARTIAL | `gas_price = 0` and normal receipts — yes. But `block.coinbase = REWARDS_ADDRESS` (enforced), the recovered tx signer is the **consensus leader's EVM address** (leader binding), and the EVM caller is `SYSTEM_ADDRESS = 0x0`. Gas is two-tier: visible intrinsic calldata gas (floor 21000) vs internal 10B limit (`system_tx.rs:82-93,670-709`, `evm/src/executor.rs:2138-2144`) |
| Per-epoch reset every 1 hour | PARTIAL | Epoch is **block-count based**: `config_epoch_length_blocks`, default 1200 (~1 h target); resets `missed_blocks`/`missed_votes`/`blocks_proposed` for ACTIVE/EXITING/JAILED (`validatorset/src/hooks.rs:12-22`, `runtime.rs:1172-1206`) |
| Proposer/validator parity via header data | MATCH | Import recomputes the execution summary and rejects on `extra_data` mismatch; leader binding re-validates system txs against header artifacts; standard state-root recompute (`executor.rs:2919-2935`, `consensus/src/application/validation.rs:116-238`) |

## 4. Validator lifecycle, staking, slashing (§4, §5.2)

| Claim | Verdict | Actual behavior |
|---|---|---|
| Six statuses (Registered…Inactive) | PARTIAL | Seven: `REGISTERED=0, PENDING=1, ACTIVE=2, EXITING=3, UNBONDING=4, INACTIVE=5, JAILED=6` (`validatorset/src/runtime.rs:24-34`) |
| Felony → forced exit, no recovery except re-registration | DIVERGED | Felony path **jails**; `unjailValidator` recovers JAILED → PENDING (cooldown + stake ≥ min) without re-registration. Force-exit (`EXITING → UNBONDING → INACTIVE`) also exists and does require re-registration (`validatorset/runtime.rs:756-768,889-928`) |
| Consensus set includes EXITING until reshare | MATCH (wider) | Current consensus participants = `ACTIVE \| EXITING \| JAILED` while a BLS share is held (`runtime.rs:80-83`) |
| `pending_set_change` signal | MATCH | Slot 25; polled by the engine as the reshare trigger (`schema.rs:123`, `engine/src/validators.rs:323-334`) |
| Churn-limited exit queue | NOT IMPLEMENTED | `enqueue_unbonding` appends unconditionally (`staking/src/logic.rs:118-139`) |
| Withdrawability 21 d normal / extended slashed | PARTIAL | Genesis-configured: `config_unbonding_period` (21 d is a comment default) and `config_slashed_withdrawal_delay` (defaults to 2× when 0) (`staking/src/logic.rs:107-116,354-372,446-450`) |
| min_stake / re-registration cooldown constants | PARTIAL | Both are genesis config (`config_min_stake` required non-zero; `config_reregistration_cooldown` default 0) — no protocol constants |
| Parameter table (150 / 500 / 1/32 / ×3 / 2²⁴ / bias 4 / churn 4 / 65536) | MIXED | 150 and 500 exist (`DEFAULT_PROPOSER_FELONY_THRESHOLD`, `DEFAULT_VOTER_FELONY_THRESHOLD`; proposer path dormant). Slash = **5%**, not 1/32. Everything else in the table — NOT IMPLEMENTED (`slashindicator/src/runtime.rs:42-53`) |
| Voter misses from cert bitmap; proposer misses from view gaps | PARTIAL | Voter: yes (bitmap over `ordered_committee`, absentees at window close). Proposer view-gap detection exists in the reporter but its output is banned from committed metadata in V2 — effectively disabled (`reporter.rs:717-736`, `verifier.rs:316-320`) |
| Staking: self-stake, unbonding queue, claims | MATCH | Self-stake only (`caller == validator` enforced); ABI: `stake`, `unstake`, `claimUnbonded`, `unjailValidator`, `getStake`, `getTotalStaked` (`staking/src/precompile.rs:26-47`, `logic.rs:25-29`) |

## 5. Emission and business modules (§14, ADR-005)

| Claim | Verdict | Actual behavior |
|---|---|---|
| Emission from `block.timestamp − genesis_timestamp` | DIVERGED | Closed-form per-day exponential decay: `day_emission_limit(day) = 2³⁰ tokens × exp(−k_soft × day)`, fixed-point Taylor series, floor 2²⁶ after day 2920; `day_number` = UTC-day difference from the genesis anchor (block-0 timestamp bucketed to a day key) (`emissionlimit/src/day_emission.rs:23-82`, `rewards/src/runtime.rs:76-122`) |
| Terminal-sink semantics (fallback, checkpoints, terminal fatal) | MATCH | `dispatch_allocations`: non-terminal sinks under `with_checkpoint`, failures/unused roll to terminal, terminal failure is fatal — but it runs in the daily Cycle handler, not per block (`emissionlimit/src/allocation.rs:127-179`) |
| AgentReward: 8%, 50/50 wallet/SFA pools | DIVERGED | Four pools, each 4%: **WAA** (wallet), **SRA** (signer-of-record-attestation), **CCA**, **Merchant**. WAA/SRA distribute by tribute count with the 32% cap + iterative redistribution (this part matches); CCA/Merchant are accumulators. "SFA" doesn't exist in code. `claimReward(0)` = claim-all — on AgentReward (`agentreward/src/distribution.rs:7,168-183`, `precompile.rs:21-32`) |
| Promis: mined from settled Intex with PoW, TransientStore, 1 MineCoen/address/block | DIVERGED | Promis is a plain non-transferable fungible token minted by **Desis auction clearing** (`promis_load` = 100k per Intex). No PoW on Promis, no TransientStore, no MineCoen rate limit. `mineCoen` is a **GratisFactory** Gratis→COEN 1:1 burn/mint. PoW (SHA-256) exists but gates **Gem/Nod factories** (`promis/src/runtime.rs:33-85`, `desis/src/runtime.rs:170-189`, `gratisfactory/src/runtime.rs:85`, `common/src/pow.rs`) |
| Intex "not yet fully implemented" | STALE | Substantially implemented, split: `intex` series ledger + **Desis** three-stage auction (Start → Reveal → Clearing, commit-entry bond) + Solidity target-chain contracts (`IntexNFT1155`, bridge, auction) (`desis/src/schema.rs:59-68`, `contracts/intex/src/`) |
| Gratis = zero-fee mechanism | CONFLATED | Gratis is a non-transferable token (escrowed to Credis). Zero-fee is the separate `crates/system/zerofee` module: protocol-tx hook registry + **EIP-7702 paymaster** with a daily free-tx quota (`zerofee/src/lib.rs:1-46`) |
| WWD: 50 h period; validity closes 36 h after end | PARTIAL | 50 h — yes (`FORMING_PERIOD_HOURS = 50`, day keyed at UTC+14). No 36 h constant: windows are `LOOKBACK_DELAY_HOURS = 502`, `OFFERING_PERIOD_HOURS = 50`, `WAITING_PERIOD_HOURS = 12`; sealing tied to OFFERING status (`metadosis/src/constants.rs:1-11`) |
| Anadosis = orchestration module | DIVERGED | No such module. "Anadosis" is the Credis installment concept: 10 monthly repayments per position (`credis/src/runtime.rs:3-31`) |
| Nod floor = issuance × 1.08 | MATCH | `FLOOR_RATE_PERCENT = 8` over `max(tribute_price, entry_price)` (`lysis/src/constants.rs:4-8`) |
| Module inventory | INCOMPLETE | Code has 24 core crates + 12 system crates. Whitepaper omitted: `cycle`, `desis`, `gem`/`gemfactory`, `credis`/`credisfactory`, `gratisfactory`, `gratispool`, `promisfactory`, `vaultprovider`, `accounting`, `oracle`, `zerofee`, `zkproof`, `tee`/`teeregistry` |

## 6. P2P, node surfaces, CLI, RPC (§8, §10)

| Claim | Verdict | Actual behavior |
|---|---|---|
| Transport TLS/Noise | PARTIAL | Commonware `authenticated::lookup` with its own namespaced encrypted handshake (BLS identity, `max_handshake_age`, per-IP quotas) — functionally equivalent, literally neither TLS nor Noise (`engine/src/stack.rs:2032-2144`) |
| Peer bans: permanent for invalid crypto; exponential backoff 5min→24h | DIVERGED | No custom scoring/ban schedule in outbe; inherited commonware blocking is a **flat `block_duration`** (recommended profile: 4 h). Rate limiting = per-channel `Quota` + handshake quotas |
| DKG complaints protocol | DIVERGED | Ack-based `feldman_desmedt` (players ack valid shares; dealers without quorum acks / with invalid logs are excluded). No complaint-with-proof broadcast. Shares protected by point-to-point routing over the authenticated channel + `DkgCeremonyId` round binding (`dkg_actor/actor.rs:272-273,1107-1133`, `wire.rs:24-58`) |
| Block propagation with proposer BLS signature | MATCH | Buffered broadcast + marshal resolver backfill; Simplex verifies leader/signatures; handler executes via Reth before voting, with the deterministic timestamp-drift clamp (`application/handler.rs:45-198`) |
| Certificate tamper resistance | MATCH (nuance) | Aggregate + bitmap (length, quorum `N3f1`, index bounds). Individual byzantine seed partials are neutralized and flagged slashable; the **recovered aggregate** VRF proof is mandatorily re-verified at the next height by the certified-parent verifier (`proof/verifier.rs:151-262`, `hybrid.rs:460-527`) |
| Light Client Stack (SDK) | NOT IMPLEMENTED (primitives exist) | No SDK crate. The proof package exists in-node: `outbe_getFinalization` RPC (certificate + block), committee registry, and the `--upstream` follower that walks finalized proofs across reshares (`rpc/src/api.rs:143-222`, `consensus/src/follow/driver.rs`) |
| Key storage backends (3) | MATCH | `Plaintext`, `Encrypted` (AES-256-GCM + Argon2id), `OsLevel` (Keychain / Secret Service) (`keygen/src/main.rs:37-45`, `engine/src/args.rs:100-114`) |
| Key tooling | MATCH | `outbe-keygen`: `generate`, `show-pubkey`, `sign-registration` (PoP over validator address), `verify`, `hybrid` |
| DKG maintenance surface | MATCH | `outbe-chain dkg bootstrap\|status\|export-share\|import-share\|force-restart` + `--testnet.force-dkg` / `--testnet.trust-el-head` (mainnet-rejected) (`bin/outbe-chain/src/main.rs:91-239`) |
| outbe_* RPC | MATCH (+1) | 12 methods: `getValidators`, `getValidator`, `getEpochInfo`, `getStake`, `getSlashInfo`, `getSlashConfig`, `getParticipation`, `consensusStatus`, `getVrfSeed`, `getEmissionInfo`, `syncStatus`, **`getFinalization`** (whitepaper/README omit the last) (`rpc/src/api.rs:156-223`) |
| Node roles: validator / full node | MATCH (+1) | Plus a third sub-mode: `--upstream` verifying follower, mutually exclusive with `--validator` (`engine/src/args.rs:136-151`) |
| EVM compatibility | MATCH (caveats) | ZeroFee admission forces pool balance `U256::MAX` for sponsored txs; deterministic priority classes outrank the tip market; reserved system-tx types rejected from public admission (`txpool/src/lib.rs:57-390`) |

## 7. In code, absent from whitepaper v1.0

Mechanisms that materially shape the protocol and had no whitepaper coverage (now added in v2.0):

1. **Cycle module** — day orchestration driving emission, WWD status advancement, AgentReward distribution, Rewards settlement; ticks at 00:00 and 12:00 UTC (`crates/system/cycle`).
2. **Gems** — validator emission instrument (Genesis/Validator gems, PoW-mineable) replacing claimable balances (`crates/core/gem`, `rewards/src/api.rs`).
3. **LateFinalizeCredits phase + K-block inclusion window** — credits late finalizers, defers absentee accounting/slashing to window close (`begin_block_precompile.rs:693-865`).
4. **JAILED status + `unjailValidator`** recovery path (`validatorset/src/runtime.rs:889-933`).
5. **TEE stack** — real enclave binary (`outbe-tee-enclave`, gramine-sgx), block-1 `TeeBootstrap` system tx with three deterministic gates (supermajority + snapshot binding, reshare membership, prior-committee endorsement), Noise-IK host-enclave channel, dedicated TEE P2P channels (`crates/system/tee*`, `engine/src/tee_bootstrap.rs`).
6. **Block-timestamp drift band** — consensus-normative `[1 s, 1 h]` advance band vs parent, proposer clamps (`README`, `application/handler.rs`).
7. **CommitteePreAnnounce artifact (tag 0x07)** — committee chaining so the outgoing committee authenticates the next set (`reshare_artifact.rs:174-178`).
8. **Sub-second timestamp artifact (tag 0x05)** — millisecond part kept out of the header for Ethereum header-hash compatibility (`primitives/src/header.rs:70-92`).
9. **Seed-partial attribution + sanitization** — `seed_partial_identity_sig`, byzantine partial neutralization, VRF evidence-slashing precompiles (`hybrid.rs`, `proof/seed_partial.rs`).
10. **Startup live-join reshare / VerifierOnly mode** — share-less nodes follow and re-key at the next reshare (`stack.rs:5984-6098`).
11. **Revealed-share exposure semantics** — offline validators' share evaluations publicly revealed in `DealerLog`; operator must rotate keys; surfaced via WARN + metric.
12. **ZeroFee EIP-7702 paymaster** at `0xEE09` with daily free-tx quotas (`crates/system/zerofee`).
13. **Slashing journal sidecar** — append-only `slashing-journal.jsonl` (`primitives/src/slashing_journal.rs`).
14. **Sybil / operational guards** — `MAX_SELF_REGISTERED_UNSTAKED = 32`, `confirmValidatorReady` stale-join gate, `max_stake_percent` concentration cap.
15. **Credis / Desis / Fidelity / Gem-GemFactory / GratisFactory / GratisPool / VaultProvider** modules (`crates/core/*`).
16. **discv5-only discovery** — DNS discovery hard-disabled (RUSTSEC mitigation) (`bin/outbe-chain/src/main.rs:429-436`).

## 8. Numbers corrected in whitepaper v2.0

| Item | v1.0 | Code |
|---|---|---|
| Block time | ~1 s-class | ~2 s-class target (README); epoch sizing comment assumes ~3 s |
| Validator cap | 128 | 256 codec cap; 128 design target |
| Initial slash | 1/32 (~3.1%) | 5% |
| Emission sinks | 4/8/4/~84 per block | 4/4/4/4/4/80 per day |
| Reshare cadence | 21000 blocks | `epochLengthBlocks`, default 1200 |
| Epoch length | 1 hour | 1200 blocks (config), ~1 h target |
| Gratis symbolic rate | 8% (fraction 8–16%) | 32% (fraction 32%, cap 64%) |
| WWD windows | 36 h validity close | 50 h forming / 502 h lookback / 50 h offering / 12 h waiting |
| `is_batchable` | true | false |
| ZK system | PlonK | Groth16 precompile; offers use TEE attestation |
