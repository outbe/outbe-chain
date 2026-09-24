# Enclave upgrades

Install the compatible node and CLI before approving a measurement update. Keep
its predecessor enclave running while provisioning the replacement. A node that
does not understand the new update payload cannot execute this rollout.

The governance payload contains `version`, `activationHeight`, `info`,
`mrenclave`, and `predecessorTeePolicyHash`. The vote CLI supplies the exact
finalized predecessor hash when omitted. Approval requires a future activation
height, one predecessor measurement, a changed MRENCLAVE, and no other waiting
software update. `mrenclave` and `teePolicy` are mutually exclusive.

The successor policy explicitly selects code-only admission with a zero MRSIGNER
field. Existing nonzero-MRSIGNER policies retain their signer constraint and
canonical hashes. Operator signing still produces a real SIGSTRUCT; the network
admits the approved code, product ID, and minimum SVN independently of the
operator's signing key. DCAP networks still require verified quotes.

## Provisioning and promotion

Candidate B needs its own enclave state directory and endpoint. Keep the same
chain/genesis and measured network descriptor. Never initialize a new network
root to recover an existing network.

1. Start B. Run `outbe-cli tee upgrade-prepare` with
   `--candidate-enclave-socket`, `--node-data-dir`, `--active-tee-dir`,
   `--candidate-tee-dir`, and the existing `--reth-p2p-secret-key` if needed.
2. Run `tee upgrade-provision` with the associated EVM owner key,
   `--candidate-enclave-socket`, `--node-data-dir`, `--genesis`, a fresh
   `--binding-id`, and `--valid-until` (consensus Unix time).
   The command persists the signed preparation, waits for finalized pending
   authorization, and obtains recipient-bound ciphertext from `outbe_upgradeKeyV1`.
   Both enclaves verify the finality and registry proofs. B seals and fsyncs the
   existing network key before confirming resident-key readiness.
3. Run `tee upgrade-submit` with the same candidate and binding ID and a fresh
   lease deadline. The registry verifies B's resident-key proof, consumes pending
   authorization, and installs B's binding atomically.
4. The running node observes its exact finalized transition, promotes B, and
   requests a restart. Point the service at B's endpoint and restart it.
   `tee upgrade-status` reports the durable checkpoint.

For a first migration from a legacy DirectDev enclave, provisioning supports
`--legacy-direct-dev-source`: the updated source node validates the current
finalized candidate before invoking the existing encrypted onboarding exporter.
This option is forbidden on DCAP networks. A DCAP donor must already support the
finalized-proof exporter; installing only a new host binary does not add that
capability to an old enclave.

`upgrade-copy-root` is retained for legacy MRSIGNER seals. Combined seals cannot
be copied across measurements, signers, or physical platforms. New network-root,
identity, and NodeHost-authorization writes use TSGX1 with an EGETKEY request bound
to both MRENCLAVE and MRSIGNER. Malformed or unauthentic blobs fail closed.

## Cancellation, deadlines, and recovery

Before provisioning, ordinary renewal may continue. Finish a pending renewal
before freezing the final transition counters. Retries retain exact signed
bytes. Expired, unexecuted transitions can be regenerated only after checking
the finalized source binding. `cancelEnclaveUpgrade` requires the exact pending
context; `upgrade-provision --new-attempt` explicitly creates a fresh preparation
after cancellation. Cancellation cannot erase ciphertext already delivered.

At activation height H, predecessor bindings lose admission even if their
leases have not expired. Under this explicit governance update, active validators
that missed the transition are jailed and slashed 10% of bonded stake and pending
unbonding amounts, once per proposal. The sweep precedes matured withdrawals.
Full nodes lose admission without staking penalties. Legacy updates do not opt
into this rule. The network removes authority; it does not remotely kill an
arbitrary enclave process.

Late owner-authorized transitions remain possible while a healthy quorum can
finalize them. Updating does not unjail a validator or refund its penalty.

With the execution node stopped, `tee upgrade-finalize` can reconcile a submitted
transition through a healthy trusted RPC, persist its catch-up anchor, and promote
B. Start a certified follower with the same datadir and network/TEE options,
omitting validator authority flags and using `--upstream` without
`--upstream.nocertify`. Wait for the durable recovery-ready log before restarting
validator authority. Readiness requires finality, the exact header, and execution
checkpoint in one committed database snapshot. Normal stake, cooldown, readiness,
and fresh-DKG requirements still apply.

A promoted journal can start another successor update. In-progress contexts
cannot be silently replaced. The existing chain identity and network key remain
unchanged throughout.

## Hardware sealing acceptance

Build `outbe-sgx-sealing-probe` in release mode with the `sgx-sealing-probe`
feature. Copy it as `probe` into a fresh private directory and run
`scripts/tests/sgx_sealing_hardware.py` from that directory using the Python
installation that provides Gramine's `graminelibos`. The script needs real SGX
devices, Gramine and OpenSSL. It seals public fixtures only.

The evidence file records the executable hash, SIGSTRUCT identities and runtime
local-report identities for every case. It checks restart recovery, rejection
under another signer or measurement, recovery after restoring the original
identity, and a DCAP quote bound to the fixture report data. Quote generation
is distinct from collateral acquisition and production QVL verification.
The optional `cross-host` invocation requires copying the same private probe
directory to another physical SGX host; a same-host run does not prove platform
isolation. This probe does not replace the four-node upgrade and Tribute tests.
