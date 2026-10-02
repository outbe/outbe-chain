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
`a7b7e375e857dd7588b8ed09b39968606ecbc9b1`, the initial implementation's circuits
revision. Temporary local path overrides were removed. The external circuits
checkout's unrelated staged skill files were preserved.

Final locked/offline verification against the published pin passed: all 172
core tests, the real EVM issuance/repayment regression, and Clippy with warnings
denied. No local Cargo dependency overrides remain.

## Circuits v0.27.0 upgrade (2026-10-02)

Implemented in this worktree on feature-branch baseline `e395ee22`.
All four circuits dependencies now use tag `v0.27.0`, locked to
`5bab266947d8e64ab07520dfcabc1462e45639af`. The lockfile updates only those four
packages; the external circuits checkout and its staged files are unchanged.

Runtime, TEE, wallet, CLI and test consumers use `pledgenote`,
`pledgenote_issue` and `pledgenote_unpledge`. Witnesses use `owner` and
`note_spend_key`; withdrawal context and balance credit use the proven public
`owner`. Serial derivation uses `note_sn`, and all note/return hashing comes
from the release's shared helpers and frozen circuits.

The release retains separate issue and unpledge profiles. Solidity entrypoints
and wallet note JSON fields are unchanged. Hash tags and circuit artifacts
changed: fresh genesis and newly generated notes/proofs are required; existing
note files must not be reused as compatible notes after this upgrade.

Verification against the locked release:

- 120 core tests passed: Credis 50, CredisFactory 23, Gratis 11,
  GratisFactory 7 and Paynote 29. Real proofs cover TEE/wallet commitment
  agreement, original-owner redemption, context binding, shared nullifiers,
  change and repayment notes.
- All 161 enclave tests passed with local socket access. The sandboxed run
  initially blocked seven transport tests; the unrestricted rerun passed.
- EVM issuance/repayment rollback regression and the real-proof Credis
  benchmark test passed.
- Workspace/all-targets check and the harness `ocomp-integration` all-targets
  check passed. Multi-node live scenarios were not executed.
- Formatting, diff whitespace checks and Clippy with warnings denied passed
  for all targets of Gratis, CredisFactory, CLI, enclave and the harness.

Verification logs: `/tmp/rights20-v027-{check,tests,enclave,evm,benchmark,workspace,harness,clippy,fmt}.log`.

## Pledge context ownership (2026-10-02)

Pledge-note operation contexts now belong to `outbe_gratis::context`:
`PledgeDomain::{Issue, Unpledge}` and `pledge_context`. The existing
`outbe_gratis::api::unpledge_context` path remains available through a re-export.
CredisFactory still encodes the complete reservation target and delegates the
operation context to Gratis. Paynote's `SettlementDomain` now contains only
Nod, Gem and Intex; Gratis and CredisFactory no longer depend on Paynote.

Domain bytes 4 and 5, the 97-byte preimage, and Keccak-to-field reduction are
unchanged. Fixed vectors captured from the original implementation verify
compatibility and separation from Paynote contexts. The zero-context rejection
now uses a pledge-specific revert. No circuits, proof formats, storage or
Solidity interfaces changed, and this refactor requires no additional genesis
reset or regeneration of v0.27.0 notes/proofs.

Verification: 65 tests passed (Paynote 28, Gratis 14, CredisFactory 23), including
context binding, real proofs, original-owner redemption, shared nullifiers,
repayment notes and rollback. Workspace/all-targets compilation, formatting,
diff whitespace checks and Clippy with warnings denied for Paynote, Gratis and
CredisFactory passed. Logs: `/tmp/pledge-context-{vectors,tests,workspace,clippy}.log`.

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
