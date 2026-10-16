# Intermediate P3: canonical public Output provisioning

Source: `a3f0c81e7`. The full SHA, parent, specification hashes, exact commands,
environment, times, exits and log hashes are in `checks.json`. This evidence does
not close P3 or any dependent task.

## Runtime wiring

The node launcher installs its concrete `RethSnapshotReader` as the network
service's `RepositoryAuthorityReader`. The interface has no block argument: each
local material request reads this node's current canonical finalized authority.
It reuses the existing exact-state/effective-set checks rather than reading
latest/pending state or accepting peer-supplied authority metadata.

The service owns a bounded watch channel for one public DKG Output. For an
enabled governments genesis, the consensus stack transfers its unique publisher
to `EpochSupervisor`; no publisher is installed for disabled chains. The engine
publishes active `last_dkg_output`, epoch and material version:

- after startup pending-boundary restoration;
- after signer activation advances the epoch;
- after dealer demotion advances the epoch;
- after pending recovery advances the epoch;
- after share-less verifier previous-output refresh from authenticated history.

`EndpointNetworkResolver::repository_signing_material` is a typed local request
over the existing bounded command mailbox, not a new peer wire message. The
service reads fresh authority on each request, checks the configured chain and
genesis, requires exact engine/finalized epoch and version agreement, then uses
`RepositorySigningMaterial::new` to check owner key, full polynomial commitment,
threshold and ordered participant mapping. During a transition mismatch it
returns an error; it cannot select an older output or caller-selected height.
Missing output, reader failure, publisher drop or closed/full mailbox also fail
without fallback. The publication does not wait for the consumer or a sidecar.

## Key boundary

The channel contains `Output<MinSig, MinPkPublicKey>`, epoch and version only.
The pinned SDK's `Output` fields are `summary`, public `Sharing`, and public
dealer/player/revealed sets (`cryptography/src/bls12381/dkg/feldman_desmedt.rs`).
It contains neither a private group key nor `Share`. No local share, provider
handle, private identity key or signing namespace is passed through this channel.
`canonical_output()` encodes these public data only. Existing threshold signing
still uses each validator's local provider; the new bridge does not reconstruct
a private key or a signature.

## Behavioral coverage

`repository_material_bridge_requires_matching_live_output_and_fresh_finalized_authority`
runs the actual network service and typed mailbox with a four-player threshold
fixture. It checks empty startup, canonical Output byte equality and material hash,
wrong chain/genesis, reader failure despite cached output, mismatched engine/state
generation, matching updated metadata, reordered participants, wrong polynomial
commitment, withdrawal, publisher drop and service shutdown. A counting authority
fixture proves each processed request reads authority again. Generation changes
in this test reuse a polynomial; this is a provisioning/freshness test, not proof
of a real reshare ceremony.

`repository_material_requests_share_the_bounded_nonblocking_local_mailbox`
fills all 256 existing command slots and confirms the next local request fails
immediately. The channel retains one current Output and has no transcript or
historical-material map. Requests use the existing fixed handle deadline.

The authority-reader implementation delegates to the existing production Reth
reader whose no-finality/noncanonical/unavailable/effective-set tests are also in
the full adapter library suite. The full engine library suite covers existing
DKG/activation/recovery and consensus stack regressions; compilation checks the
real node launcher and dependent packages.

## Outstanding

This is the in-process public material bridge. It is not the protected node ↔
sidecar signing protocol or the distributed collector. No live multi-validator
reshare/publication, secret-share transport, native automatic bootstrap, durable
journal, complete package/receipt ticket, Merge or feature E2E claim is made.
Those approved requirements remain open. No independent acceptance PASS is
claimed. There are no Heartwood, `git.rs`, fetch-gate or D2 source changes.
