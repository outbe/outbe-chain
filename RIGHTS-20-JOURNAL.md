# RIGHTS-20 implementation journal

Working directory: `/Users/oleg/.codex/worktrees/acf5/outbe-chain3`.
Chain baseline: `10d919ef` (feature branch baseline, detached in this worktree).
Fresh genesis is required: storage layouts and ABIs changed.

## Implemented

- Owner-bound pledge notes share one commitment tree and nullifier set across issue and unpledge.
- Operation contexts bind full stored reservation terms, or chain/factory/destination/withdrawal amount. Nullifiers exclude context.
- Reservation freezes quote, asset metadata, policy rate, reference currency and call terms for 15 minutes. Interest starts at issuance.
- Issuance stores a proof-authenticated return serial; repayment appends notes with cumulative-release receipt contexts. Equal repayments produce distinct notes. Interest-only payments produce none.
- Encrypted Gratis backing moves between source balance, unspent notes and the Credis aggregate balance. No per-source pledged balances or owner-revelation tickets remain.
- Repayment and forfeiture preserve Fidelity blobs. Forfeiture burns only the position's remaining collateral.
- Runtime checkpoints cover proof consumption, notes, balances, positions and transfers. ERC20 return values and reentrant position changes are checked.
- CLI note/proof preparation, MCP transaction tools, Solidity interfaces and precompile ABI exports updated. RPC harness and benchmark migrated and compiled.

## Verification (2026-10-01)

- Credis arithmetic/state tests: 50 passed.
- CredisFactory real-proof tests: 23 passed, including frozen terms across midnight and storage-access isolation.
- Gratis: 11 passed; GratisFactory: 7 passed; VaultRouter: 52 passed; Paynote: 29 passed.
- Enclave: 161 passed (transport tests require local socket access).
- Real EVM issuance/repayment regression: passed. Covers false/reverting ERC20 calls, failed approvals and vault deposits, reentrancy, metadata changes, stake/proof errors, replay, expiry and rollback of positions, notes, logs and transfers.
- Credis benchmark: passed with real proofs and encrypted backing.
- MCP: 26 tests passed; TypeScript check passed; precompile ABI export passed.
- Entire Rust workspace/all-targets check passed; RPC harness integration-feature check passed.
- Clippy passed with warnings denied for all changed core crates and CLI.
- Noir pledge library test passed. Existing freeze tooling reproduced all active circuit artifacts.
- Full multi-node live RPC scenarios were compiled, not executed.

## Circuits dependency

Pledge profiles were developed from v0.26.0. Both ACIR loaders needed to trim
trailing whitespace in the frozen base64 bytecode. The fix is now published in
`a7b7e375e857dd7588b8ed09b39968606ecbc9b1`, which is the chain's pinned circuits
revision. Temporary local path overrides were removed. The external circuits
checkout's unrelated staged skill files were preserved.

Final locked/offline verification against the published pin passed: all 172
core tests, the real EVM issuance/repayment regression, and Clippy with warnings
denied. No local Cargo dependency overrides remain.

## Privacy and accounting

`pledged_total = unspent_note_backing + Credis_collateral_balance`;
`Credis_collateral_balance = sum(position.collateral_locked)`.

Initial/change/repayment notes have the same commitment format. Issue hides the
source; unpledge proves the public credited address owns the private input note.
Public amounts, timing, reservations, repayment events and OCOMP snapshots can
still correlate activity. This change does not provide amount/timing anonymity.
Forfeiture deliberately leaves Fidelity cohorts intact.

## Wallet workflow

Amounts below are raw token units. Keep note files and issuance receipts private;
they contain the secrets needed to redeem change and repayments.

```sh
outbe-cli pledgenote prepare OWNER GRATIS_AMOUNT NONCE MODIFY_KEY_FILE
outbe-cli pledgenote issue-proof NOTE_FILE RESERVATION_ID
outbe-cli pledgenote return-note ISSUE_RECEIPT POSITION_ID RELEASED_AMOUNT CUMULATIVE_RELEASED
outbe-cli pledgenote unpledge-proof NOTE_FILE WITHDRAW_AMOUNT
```

`prepare` saves the initial note before printing its commitment and ModifyAuth.
Submit `pledgeGratis(amount, auth)`, then generate the reservation-bound issue
proof. `issue-proof` saves an issuance receipt and optional change note. Submit
its proof bytes with `issueCredis(reservationId, proof)` and the exact native COEN
stake. On repayment, derive the return note using the saved receipt and verify
its commitment is on chain before generating an unpledge proof. Repayment itself
requires no note proof and cannot redirect the return serial.

MCP exposes `credis_reserve`, `gratis_pledge`, `credis_issue`, `credis_settle` and
`gratis_unpledge`; proof generation and modify keys remain local to the wallet.
