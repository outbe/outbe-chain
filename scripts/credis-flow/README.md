# Credis flow

The demo uses the PledgeNote APIs and offline Rust encryption. See
[the protocol and CLI guide](../../PLEDGENOTE.md) for request JSON and lifecycle.
Build `outbe-cli`, put it on PATH (or set `OUTBE_CLI`), then run `npm ci` and
`npm run build` here. The generated ABIs come from the repository's Solidity exports.

Prepare create/cancel/use calldata on the owner's machine with
`outbe-cli pledge-note prepare`. Submit only those encrypted files:

```sh
npm run pledge-gratis -- /path/create-encrypted.json local
npm run request-credis -- /path/use-encrypted.json local
npm run unpledge-gratis -- /path/cancel-encrypted.json local
```

Create/cancel use `RELAYER_PRIVATE_KEY`; issue uses `CCA_PRIVATE_KEY`. The relayer
must differ from `USER_ADDRESS`. Both use the environment's `RPC_URL`. The scripts
validate the prepared target/selector and preserve the exact native stake value.
Create/cancel write a new `.receipt.json` containing encrypted receipt bytes.
Add the view key locally and decrypt with the CLI. Issuance saves only a public
position receipt in `tickets/`; the note and spending secret stay with the owner.

`setup-native`, `setup-erc20`, `setup-gratis`, smart-account funding/purchase, and
`user-settles` retain their existing roles. `info`, `setup-gratis` and `user-settles`
query the new confidential ledger through the Rust CLI using owner-derived keys.
Private query inputs use temporary owner-only files, removed after the CLI returns.
Old pledge tickets and old account-keyed ciphertext are incompatible with this
from-genesis protocol version. The source account is never an issuance argument.
