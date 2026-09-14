# Rudis GEM wallet script

Default RPC: `https://125.253.92.5`, Rehearsal chain ID `70860602`.
WUSDC: `0xdD9eD2f161c4F9A642471BCF49331A95F5B2B1d3`.

From the repository root, install the TypeScript dependencies:

```bash
cd scripts/credis-flow
npm ci
```

List the wallet's GEM IDs, PROMIS amounts, WUSDC settlement quotes, states and
WUSDC/native balances:

```bash
npm run rudis-gems -- list --private-key "$PK"
```

The read-only command also accepts `--address 0x...` instead of a private key.
Amounts use integer arithmetic and token decimals. `Issued` GEMs show a quote
but cannot settle yet; `Settled` GEMs show `0 (paid)`. A failed contract quote is
reported as unavailable, never as zero. Reads use one block for the list.

Settle one GEM and convert its PROMIS to native RUDIS (called COEN in the main
Outbe interfaces):

```bash
npm run rudis-gems -- settle --gem-id 0x... --private-key "$PK"
```

`--gem-id` accepts decimal or hexadecimal uint256. The script performs:

1. Owner, state, deadline, chain and quote checks; enclave key derivation through
   `rudis_deriveKeys`, signed locally using the supplied private key.
2. WUSDC approval/deposit through the canonical Rust PayNote CLI, generating a
   local bearer note and a spend proof, then `settleGem(gemId, proof)`.
3. PoW search: SHA-256 of `gemId_be32 || nonce_be8` with one leading zero byte.
   A mint MAC binds the owner's Promis modify key, mint operation, exact GEM load,
   current Promis op-nonce and chain ID. It calls `minePromis`.
4. After the mint receipt, it reads a **fresh op-nonce**, computes the burn MAC,
   and calls `mineRudis`. Only this GEM's amount is converted; existing PROMIS
   remains. Protocol-6 PROMIS converts to native-18 RUDIS at 1:1 in whole tokens.
5. Successful receipts and their GEM/RUDIS events are checked; final balances
   are read back. Transaction hashes are printed for each step.

No private key is sent to RPC or saved in the operation journal. Key-derivation
responses are sealed to a locally generated ephemeral X25519 key. The original
`outbe/*` cryptographic domain tags remain unchanged on Rudis.

## PayNote prerequisite

`list` and `--dry-run` only require Node.js (20+ recommended) and npm dependencies.
Actual settlement also needs a built `outbe-cli` with `paynote deposit` and
`paynote spend-proof`, plus its working Barretenberg/SRS proving setup. The CLI
must use the PayNote circuit version accepted by the deployed node. The script
reuses this prover rather than implementing a second ZK protocol.

Build the CLI from the repository root using the repository's Rust/ZK build
environment:

```bash
cargo build --release -p outbe-cli
```

The script finds `<repo>/target/release/outbe-cli`, then `outbe-cli` on `PATH`.
To select an existing compatible binary:

```bash
npm run rudis-gems -- settle --gem-id 0x... --private-key "$PK" \
  --outbe-cli /path/to/outbe-cli
```

The native wallet balance must cover transaction gas in addition to the WUSDC
settlement amount. Approval is limited to the required deposit by the Rust CLI.

## Preview, cost cap and recovery

```bash
npm run rudis-gems -- settle --gem-id 0x... --private-key "$PK" --dry-run
npm run rudis-gems -- settle --gem-id 0x... --private-key "$PK" --max-settlement 30
```

`--max-settlement` is a WUSDC amount in whole-token decimal notation and excludes
native gas fees. A dry run reads the owner, state and current quote; it does not
prove a PayNote, derive confidential keys or simulate the complete transaction
sequence. `--rpc-url` overrides the URL but the chain ID must remain `70860602`.

Run the same settlement command again to resume. Keep the default ignored
`<repo>/.rudis-gems/` directory (or the path selected with `--state-dir`). It holds
owner-only bearer notes, proofs and an operation journal, partitioned by chain,
wallet and GEM. Never commit or share these recovery files. Signed transaction
bytes are saved before broadcasting: after a timeout the exact hash is checked
and the exact transaction may be rebroadcast, without creating a second spend.

The script reuses a previously deposited PayNote. If a deposit is unconfirmed or
interrupted, it stops rather than depositing again. Inspect the printed deposit
hash and the saved note's `hasCommitment` state before manual recovery. If the
price grows beyond the deposited note amount, the script stops and preserves
the note. A lower quote may create a change note under `paynotes/`.

An abrupt process kill may leave `run.lock` in the printed operation directory.
After checking that the old process has stopped, remove **only** `run.lock` and
repeat the command. Do not delete the operation journal to retry a pending tx.

## Validation

```bash
npm run test:rudis-gems
```

Tests cover uint256 IDs, settlement deadlines, persistence before broadcast,
timeout/restart without duplicate submission, reverted receipts, PoW, mint/burn
MAC domains and op-nonces, and enclave key-envelope authentication/decryption.
The read-only list and fresh-wallet `rudis_deriveKeys` path were also checked
against the public RPC. A funded end-to-end settlement was not executed.
