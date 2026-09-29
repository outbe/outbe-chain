# PayNote merging (RIGHTS-39)

`mergePayNotes(bytes)` consolidates 2–4 notes of the same exact ERC20 asset into
one ordinary PayNote. Any account may relay the proof. Amounts and the total stay
private; the circuit proves positive canonical U256 values and an overflow-free
sum. The caller pays gas. No token, Reserve, Oracle, or rights contract is called.
Settlement behavior (including RIGHTS-17) is unchanged.

The new `outbe.paynote.merge@1.0.0` profile has ten public words, in order:
chain ID, pool address, root, input count, asset, four nullifiers, output commitment.
Inactive inputs and witnesses must be zero. Active inputs prove membership,
distinct commitments/nullifiers, and bearer-key knowledge. The output uses a
fresh key. Commitment, nullifier, and tree formulas are shared with the existing
spend circuit; its frozen `paynote@1.2.0` artifacts are unchanged.

Runtime rejects stale roots, spent inputs, duplicate output commitments, and a
full tree before mutation. One checkpoint marks every input spent, appends the
output, and emits `NotesMerged` and ordinary `NewNote` (amount zero denotes a
confidential amount). Settlement and merging share the same spent map. No storage
migration is needed. Public nullifiers link the inputs of a merge to each other;
this is not a claim of additional anonymity.

## CLI and recovery

```sh
outbe-cli paynote merge-proof NOTE1.json NOTE2.json
outbe-cli --private-key "$PRIVATE_KEY" paynote merge NOTE1.json NOTE2.json NOTE3.json
outbe-cli --private-key "$PRIVATE_KEY" paynote merge --resume ./paynotes/merges/0xOUTPUT.json
outbe-cli paynote status
```

The normal global RPC option selects the node. `merge-proof` accepts 2–4 inputs;
`merge` accepts larger selections and submits sequential batches (four initially,
then the previous output plus up to three inputs). The whole selection is checked
for duplicates, asset/domain mismatch, and U256 overflow before starting.

All input and output bearer notes and an immutable recovery operation are saved
durably before proving or broadcasting. Keep `paynotes/merges` private: it contains
bearer keys. Only the artifact under `paynotes/proofs` is suitable for a relayer;
it contains no amounts, input commitments, keys, or local paths. CLI output can
contain local paths and amounts and is not that public artifact.

Status reads one canonical block snapshot and detects a changing block hash.
Pending operations reserve inputs locally. Resume reconciles canonical commitments
and nullifiers, skips completed stages, and regenerates proofs for unfinished
stages. Failed broadcasts, missing receipts, expired roots, and reorganizations
never delete saved keys. A conflicting spend requires a new selection.

## Validation and measured cost

Tests cover malformed and mutated proofs, all public bindings, U256 boundaries,
unused witness padding, replay and settlement races, rollback after every storage
mutation/event, durable recovery, and public artifact privacy. Real proofs exercise
ordinary settlement through Nod, Gem, and Intex. The EVM flow merges 12 + 8 + 5,
settles 20, and preserves ordinary change of 5.

The frozen merge profile uses a 2^15 domain, 262 proof words, and 8,708 bytes for
the combined proof. A paid four-input EVM transaction used 1,504,648 gas in five
local samples, with 8,804 bytes of calldata. The precompile base charge is
1,150,000 gas plus storage metering and normal transaction charges. These are
local regression measurements, not production latency guarantees. The backend
initializes at least the existing canonical SRS capacity so a spend proof cannot
initialize a smaller one-shot SRS and subsequently break merge proving.

## Companion checkout

This implementation currently uses the sibling `../outbe-circuits` checkout in
the workspace dependencies. Both repositories are required to build it. Before
publishing the chain change independently, release the reviewed circuits changes
and replace all four sibling dependencies with the same immutable upstream
revision/tag; regenerate Cargo.lock. No release or remote publication is performed
by this local implementation.
