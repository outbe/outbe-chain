# Tribute ZKP readiness

Review baseline: `6b6b80b6` (L2 inbox key resolution).

## Registration and the two keys

Every `offerTribute` requires a registered **L2 chain id**, a valid BLS signature
over its 32-byte Merkle root, and a real proof. The offering account need not be
the registered L2 administrator. There is no unregistered/disabled bypass.

Registration is a validator-governance proposal targeting L2Registry
(`0x000000000000000000000000000000000000EE0E`), for example:

```json
{
  "operation": "register",
  "chainId": 57005,
  "l1Address": "<inbox contract address on this Outbe chain>",
  "publicKey": "0x"
}
```

The proposal must pass voting and be applied after its deadline. `publicKey`
is required in the JSON; `0x` or 256 zero bytes means resolve it from the inbox.
Registering the address does not call or validate the getter. A broken inbox can
therefore be registered, but subsequent key reads and offers revert.

* **BLS root-signing key:** either pinned at registration or fetched on each
  check by a 100,000-gas STATICCALL to `IDaInbox.groupPubKey()`. The getter must
  return ABI `bytes` containing a nonidentity 256-byte EIP-2537 G2 point. The
  signature is 48-byte BLS MinSig under `_PSO_CHAIN_COMMITMENT_ROOT`. Live keys
  are not cached. A pinned key takes precedence over the getter.
* **ZKP verification key:** compiled into `outbe-zk-canonical`, selected by the
  exact L2 chain id and circuit version. It is **not** fetched from the inbox.
  Registering an arbitrary L2 does not install a circuit binding. Devnet alone
  permits extra fixture L2 ids to reuse the 57005 binding.

The inbox is called in the local EVM, not through an external Ethereum RPC.
The registered address is also the administrator allowed to update/remove the
record; a contract administrator needs an appropriate forwarding method if it
is to perform those actions. `updatePublicKey` pins a valid nonzero key and
does not currently support switching back to live lookup.

## Genesis

`scripts/create_genesis.py` and `scripts/seed_genesis.py` do not automatically
register a test L2, install a test inbox, or provision its signing private key.
The presence of the 57005 circuit in a binary does not create its registration.
`release/testnet-genesis.json` is a template with an empty allocation.

The main Tribute-to-COEN e2e registers L2 57005 through governance after
bootstrap, with an inbox contract and no pinned key. All its Tribute offers,
including the post-activation V2 offer, select this same network explicitly.
The separate capacity-fixture builder
`seed_capacity_operator_l2_registrations` seeds deterministic **pinned** keys in
genesis for its bulk owners; that is not the ordinary genesis-generation path.

To accept a real Tribute after ordinary bootstrap, supply:

1. An approved L2 registration and either a pinned BLS key or a working inbox.
2. A compiled circuit binding and an exact supported version (the basic demo
   fixture uses L2 57005 / `1.1.0`).
3. A matching proof and signed Merkle root from the L2, plus an encrypted draft
   whose NFT/binding hashes agree with the proof and execution context.
4. A working TEE offer session/key, the verifier CRS, a day in OFFERING, and
   usable Oracle prices/currencies. Genesis registration alone cannot provide
   those runtime conditions.

Test-network convenience seeding should be an explicit option, with matching
inbox code/key material and a supported circuit id, rather than a silently
installed publicly known signing key on every network.

## Coverage and reproduction

Existing unit/EVM tests cover governance, key validation/rotation, owner checks,
uncached lookup, pin precedence, no-code/reverting/malformed/out-of-gas getters,
STATICCALL write protection, registration/proof requirements and circuit binding.
The EVM inbox tests deliberately stop at `ZkProofRequired`; they alone do not
prove successful issuance.

This change adds:

* EVM rejection of correctly ABI-encoded zero/invalid points and recovery after
  the inbox returns a valid key again.
* A real-proof TributeFactory test using a stubbed inbox response and the actual
  enclave computation, checking issuance, replay, rewards and rollback.
* `@tribute-inbox-key` selects the main `@ocomp-public-apply` scenario in
  `features/ocomp.feature`: a signed CREATE deploys an inbox, governance registers
  it with `publicKey: "0x"`, and a user submits an encrypted Tribute with a real
  generated ZKP. That same Tribute proceeds through OCOMP/Lysis, NOD qualification,
  payment through PayNote, Gratis issuance and redemption into exact native COEN.
  The later V2 Tribute uses the same L2. No L2 registration is performed by an
  offer step. The RocksDB storage scenarios share this explicit prerequisite.
  Focused unsigned/invalid-proof rejection scenarios remain in `tribute.feature`.

```sh
cargo test --locked -p outbe-l2registry -p outbe-tributefactory --lib
cargo test --locked -p outbe-evm --test l2_registry --test l2_registry_inbox
OUTBE_BB_SRS_PATH="$PWD/.bb-crs/bn254_g1.dat" cargo test --locked -p outbe-tributefactory --lib -- --ignored
cargo test --locked -p outbe-e2e-harness --lib features::l2_zk_gate
SOURCE_DATE_EPOCH=0 cargo test --locked -p outbe-e2e-harness --features ocomp-integration --lib env::tests
OUTBE_BB_SRS_PATH="$PWD/.bb-crs/bn254_g1.dat" SOURCE_DATE_EPOCH=0 cargo test --locked -p outbe-e2e-harness --features ocomp-integration --lib proven_offer_is_a_valid_demo_tribute_proof_for_its_statement -- --ignored
# On a provisioned SGX host with the harness binaries and services:
mise run e2e-sgx --tags '@tribute-inbox-key'
```

The cryptographic tests are marked ignored because they require the pinned CRS;
ordinary `cargo test` does not execute them. Run the explicit command above.

The frozen Metadosis day was removed from `seed-testnet-lowstake.json`.
The runtime creates the first worldwide day at block 1, as for the other testnet
profiles; genesis no longer installs an OFFERING day without a formation record.

Local validation: L2Registry/TributeFactory unit tests passed; all ten EVM
registry/inbox tests passed; all four ignored real-proof tests passed using the
local pinned CRS. The harness compiled with `ocomp-integration`; all 23 environment
and Gherkin step-registration tests passed. The harness real-proof test also
passed: the original proof verifies, while changing its binding hash to another
caller, draft, host chain or L2 chain fails verification in all four cases.
Formatting and diff whitespace checks passed.
The genesis creator suite passed 74 tests with two skipped. The separate seed
profile suite passed all 13 tests.
Those local checks do not constitute a full SGX scenario run. This host has SGX
available through sudo; live acceptance must use the release `sgx-no-attest` lane.

## Live SGX acceptance

The complete `@tribute-inbox-key` main scenario passed on branch
`test/l2-inbox-tribute-zkp-coen`, based on `main` at `6b6b80b6`.
Run `run-1790255867-2206550` completed all **53 steps**, with process exit code 0,
`result: passed` in the run manifest, and a clean audit of 37 runtime log files.
Scenario runtime was 826,532 ms (about 13 minutes 47 seconds), excluding builds.

The run started four release validators with real Gramine SGX enclaves, deployed
the inbox through CREATE, registered L2 57005 through governance with no pinned
key, generated and accepted a real Tribute ZKP, and completed OCOMP/Lysis/NOD.
It also verified FullNode synchronization, V1/V2 execution, restarts, historical
replay, contributor payouts, and the original owner's PayNote → Gratis → COEN
redemption. The owner received 32 COEN, with the native balance change checked
exactly after gas. Redemption transaction:
`0x7db87abcc58d35a64709f578d7a84d87f4bbfa989c549a267c464d6c276dae5e`.
AgentReward claims, validator Gem → Promis → COEN redemption, and third-party
ERC20 settlement of the second public Nod also passed.

Live execution exposed and corrected two harness defects:

* The inbox deploy helper must explicitly set `TxKind::Create`; an absent `to`
  field otherwise fails Alloy wallet completion before transaction submission.
* Nod qualification must wait for a completed full UTC holding day after
  `issuedAt`. A high price on the partial issuance day does not qualify a Nod.
  Both settlement paths now use real feeder publications and controlled day
  transitions, checking qualification on every validator without state injection.

After each correction only the release harness was rebuilt. All ten reused
artifact hashes were verified unchanged; previous manifests and refresh
provenance were retained. The full scenario was restarted from a fresh network.

Evidence retained on this host:

* Console log: `/tmp/l2-inbox-coen-e2e-third.log`.
* Run manifest: `/tmp/l2-inbox-coen-e2e/evidence/run-1790255867-2206550/run-manifest.json`.
* Scenario evidence: `/tmp/l2-inbox-coen-e2e/evidence/run-1790255867-2206550/scenario-001.json`.
* Chain data and runtime logs: `/tmp/l2-inbox-coen-e2e/run-1790255867-2206550/`.

```sh
OUTBE_BB_SRS_PATH="$PWD/.bb-crs/bn254_g1.dat" \
OUTBE_TEE_IO_TIMEOUT_SECS=300 RAYON_NUM_THREADS=4 \
target/release/outbe-e2e \
  --artifact-manifest target/e2e-artifacts/sgx-no-attest.json \
  --tee sgx-no-attest --sudo --validators 4 --all \
  --tags '@tribute-inbox-key' --no-cleanup --debug \
  --data-dir /tmp/l2-inbox-coen-e2e
```

Use a manifest matching the executable checkout; the report addition after this
run is documentation only. The retained run manifest records the executed source
and exact binary hashes.
