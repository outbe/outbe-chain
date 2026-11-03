# Validator onboarding lifecycle

This page describes how one `outbe-chain` process takes a validator from an
empty datadir to signing (OIP-00337). The process does not restart between
phases.

## Phases

The node reports its phase in `outbe_consensusStatus.mode`:

| `mode` | Meaning |
|---|---|
| `follower` | Certified follower (`--upstream` without `--validator`). |
| `upstreamBootstrap` | Validator (`--validator --upstream`) that syncs certified blocks from `--upstream` until it is bootstrap-ready. |
| `verifier` | Validator stack running with a verifier scheme (either because the node has no threshold share for the epoch or because the Radicle approval gate withholds signing), or a `--validator` node before Simplex starts. |
| `signer` | Validator stack running with an active signer scheme (`radicle_signer == true`). |

`isValidator` is `true` for `upstreamBootstrap`, `verifier` and `signer`.
The `phase1VerificationMode` field no longer exists.

## Bootstrap-ready

A validator is bootstrap-ready at its local finalized height `H` when one of
these conditions holds:

1. `H == 0` and the local consensus key is in the genesis `ValidatorSet`.
2. `H >= 1` and all of these facts hold:
   - The local chain has a finalized `BoundaryOutcome`.
   - The on-chain offer key is not zero.
   - The local TEE lease guard is `Armed`.
   - The enclave holds the on-chain offer key.
   - The local consensus key is in the consensus set or in the admitted
     non-consensus set at `H`.

A bootstrap-ready validator starts the validator stack at once, also when
`--upstream` is set. A validator that is not ready needs `--upstream`. It then
runs `UpstreamBootstrap` until it is ready at or above the upstream tip of
its start. Without `--upstream`, startup stops with
`ValidatorBootstrapRequiresUpstream`.

## Genesis DKG

The genesis ceremony is `n`-of-`n`, so all founding validators must be online.
Each founder contributes a `DealerLog`. A threshold subset is not enough:
interactive bootstrap has no chain carrier, so a partial dealer set can
produce a different public polynomial on each validator.

`DKG_TIMEOUT` is 120 seconds. If a `DealerLog` from every founder is not in
before that deadline, the ceremony fails with `CeremonyTimedOut`. The error
carries `missing_dealers`, the founder public keys that did not deliver a
finalized dealer log.

## Join a running network

1. Run `outbe-cli tee join` against the enclave and the upstream RPC.
2. Register and stake the validator on chain.
3. Start the node once:

   ```text
   outbe-chain node --validator --upstream <certified-rpc> \
     --consensus.signing-key <path> --validator.evm-key <path> \
     --radicle.control-socket <path> --radicle.status-address <loopback:port> ...
   ```

4. Wait for `mode` to change from `upstreamBootstrap` to `verifier`.
5. Confirm readiness on chain. The next DKG reshare changes `mode` to
   `signer` in the same process.

To restart, use the same command. A bootstrap-ready node skips
`UpstreamBootstrap`.

`--validator` rejects `--upstream.nocertify`. `UpstreamBootstrap` verifies
every certificate through the committee chain.

## TEE lease guard

The guard has three states: `AwaitingAnchor`, `Pending(anchor)` and `Armed`.
Only `Armed` satisfies bootstrap readiness.

- A durable join anchor at or below the local finalized height validates
  locally at startup.
- Without a durable anchor at `H >= 1`, the guard starts `Armed` when the
  local admission at the latest finalized block is `Ready` and the enclave
  holds the on-chain offer key. No upstream is necessary.
- Otherwise, with `--upstream`, the node checks its NodeHost admission and
  offer key on the upstream. A durable join anchor must not be above the
  upstream anchor, and the upstream hash at the durable height must equal the
  durable hash. The guard then waits at the higher anchor and arms only after
  both anchors match the local canonical chain.
- Without `--upstream`, an unarmed guard stops startup with
  `ValidatorRecoveryCatchUpRequired`.

The guard wakes on every local finalization.

## Threshold material

The node reads threshold material only from `--consensus.keys-dir` or from
in-process DKG. A validator that is not a player of the latest finalized DKG
boundary runs as a shareless verifier with public material from that
boundary. The flags `--consensus.signing-share`, `--consensus.public-polynomial`
and `--consensus.dkg-output` no longer exist.

`outbe-chain dkg bootstrap` writes a complete keys directory for each
validator in `<output>/validator-<i>/dkg-keys/`.

## Retired on-chain Governance tracker

The normative-text Governance precompile at
`0x0000000000000000000000000000000000001018` is retired and must not be reassigned.
New genesis output does not install its marker or seed canon/meta-canon storage.
Its OIP/GIP records, ABI and vote target are no longer served. Generic Vote,
Update, CodeRegistry/Radicle and other committee-controlled targets remain.

Use a new genesis, or complete every pending Vote proposal targeting the retired
address with the old binary before the coordinated cutover. There is no legacy
handler, automatic proposal migration/cancellation or historical execution
compatibility. An old pending target cannot be resolved by the new Vote registry;
simply waiting for its deadline on the new binary is not a migration strategy.
Removing the module does not erase existing chain storage.

Before an existing-state cutover, page through
`IVote.listProposalsByStatus(Pending, index, count)` and inspect each
`IVote.getProposal(id).targetModule`. None may point to the retired address.

Remove the old `canon_dir` profile setting and `--canon-dir` seeder argument.
Bundled canon/meta-canon seed assets are no longer used. These changes concern the
on-chain text tracker, not external OIP tooling or the use of validator votes to
authorize code and contract changes.
