# P5 candidate evidence

Status: implementation candidate ready for three independent full-AC reviews. **P5 is not closed.** This bundle does not claim completion of P2/P3/P4/P6/P7/P8 or native network signing.

Task: `outbe-chain-pjz.6`; source SHA `91644eb78148e7b81cd8e46478dfb0466d321e27`; P5 base SHA `8fe62b5dd0e625cd65297479cfa687e3e648ad41`. P0/P1 dependencies are closed. The base separately checkpoints unfinished P2 entry-policy wiring; P5 adds no D1 implementation. The original D1 approval request was withdrawn, not approved.

Spec file SHA256: `5d26f9ef602867a7ed86ed9954d32f4379ec5fb867fa925208db9aa36a9b0412`. Exact P5 task snapshot: `spec.json`, SHA256 `6989338f54b923a0d74a7508ad67e406a28ec631d04faa991c6ecb3bc08b33d5`. All original AC and design.md §6 apply. Source is frozen; reviewers may write only their own report/artifacts outside this candidate directory.

## Behavior

Canonical V1 JSON binds nonzero repo/patch/revision OIDs, action `accept`, target `main`, expected target head. Version/action/branch, unknown/duplicate fields, noncanonical bytes and oversized input reject. Registry membership is checked inside proposal reservation, so unknown-repo failure rolls back allocated proposal and electorate state.

The actual EVM target registry now contains RadiclePatchVoteTarget. At admission its electorate selection is persisted per proposal in appended Vote slot13; old targets and absent legacy values retain ACTIVE-only behavior. Patch cast/tally/read views use the current ACTIVE intersection with effective consensus membership. Both numerator and denominator use that same list. Existing `yes*3 >= count*2` rule remains.

An Approved callback creates one immutable payload/terminal-height/timestamp record and separate Pending execution stage. Replay preserves the first record; substitution fails. No Git, network or signing work occurs in this callback. ABI exposes immutable approval separately from execution stage. Review/Merge are later stages; Pending is not a claim of Merge.

The consumer reader checks requested block number/hash against canonical finalized history, reads state by that exact hash, and returns only a matching terminal Approved decision. No latest/pending fallback. The fixture gives different immutable state to each hash and makes fallback APIs panic. Accepted approval remains identical after a validator is jailed; historical permissions are not revoked by membership changes.

## Actual checks

`commands.json` records exact argv/cwd/env/toolchain/platform, UTC start/end, exits and log hashes. Environment is local macOS arm64/Rust1.96.0; no remote CI or Linux/SGX result is claimed.

- **505 Rust tests PASS, zero ignored:** EVM269, Governance44, Radicle34, RadicleRegistry17, Update76, Vote65. Command covers all six packages' lib suites together; no test filter. `regressions.log` contains real EVM Approved/Expired traces and exact createProposal calldata, plus the n256 gas result.
- **13 genesis layout tests PASS**, preserving existing seeded slots; appended registry approval map starts6 and execution stage9; Vote electorate starts13.
- Package **format PASS** and actual **Solidity-generated ABI equals exported JSON**.
- Affected packages' all-target **Clippy --no-deps -D warnings PASS**, exact command/result in `targeted-clippy-command.json` and `targeted-clippy.log`.
- Broad dependency Clippy returned101 at unchanged `crates/core/tribute/src/runtime.rs:99` (`collapsible_if`). This is preserved as FAIL in `commands.json`/`clippy.log`, not relabelled green. `unchanged-clippy-dependency.json` proves identical file bytes in base and candidate; a full baseline checkout execution is not claimed. No unrelated source edit or lint suppression was used. The required changed-code lint is the successful scoped command above.

At n256 with171 Yes, the storage-meter test observed max cast1,121,500 and terminal1,228,600 gas, below asserted2M/5M limits. This is the production-shaped storage meter, not a measured256-node network or full cold-storage EVM benchmark. A separate actual EVM transaction scenario executes register/propose/ballots/getters and the real pre-execution Vote hook at n3, for both Approved and Expired.

## AC to evidence

| Criterion | Source anchor | Behavioral proof in regressions.log |
| --- | --- | --- |
| AC1 — exact revision and one immutable permission | `crates/system/radicleregistry/src/vote_target.rs:51` | abi_vote_approves_exact_revision_once_only_after_deadline; actual EVM Approved/Expired cases in patch_approval.rs; exact-subject payload roundtrip/tamper tests |
| AC2 — current target-specific electorate | `crates/system/vote/src/state.rs:187` | outsiders_pending_and_retained_members_cannot_vote_or_inflate_denominator; tally_rechecks_current_membership_and_expired_decision_grants_no_permission; pending_joiner_votes_only_after_activation_and_changes_current_quorum; EVM outsider ballot rejected |
| AC3 — payload/schema/ABI/handler and bounded deterministic execution | `crates/blockchain/evm/src/handlers.rs:36` | actual EVM precompile calls + real begin-block hook; schema slots pinned; format/ABI/genesis-layout checks; maximum_electorate_has_bounded_cast_and_tally_storage_gas; no Git/signer/client imports in target handler (source inspection) |
| AC4 — failed/nonfinalized proposal cannot authorize signing | `crates/blockchain/radicle/src/manager/snapshot.rs:43` | approval_row_alone_does_not_authorize_pending_expired_or_error_proposals; unknown_repository_reservation_rolls_back_proposal_and_electorate; every_terminal_mutation_failure_rolls_back_approval_status_and_events; exact_finalized_state_does_not_leak_latest_approval_or_revoke_accepted_history; missing_finality_noncanonical_and_unavailable_snapshots_fail_without_fallback |

`source-map.json` supplies additional actual source/caller/test line anchors. The complete 27-file P5 changed-files list is in commands.json. Main wiring includes registry payload/schema/runtime/target/ABI; Vote hook/admission/cast/tally/views; EVM registry and actual transaction/hook fixture; manager exact-state reader; Cargo dependencies/lockfile and preserved layout fixtures. No consumer signature API was silently defaulted to a different electorate.

## Negative results and acceptance boundary

Counters and authoritative state are unchanged after unknown-repo admission failure. Failure after every terminal write/event rolls back approval, Vote status and logs. Duplicate voter and revision substitution reject. No approval is exposed for Pending/Expired/Error, even if a malformed caller had installed an approval row. Provider absence, wrong canonical hash, absent finality and unavailable exact state deny without fallback. Joining/jailing during Pending changes the current electorate; a retained effective signer without ACTIVE status cannot vote or inflate the denominator. Normal Update/Governance/L2/Factory behavior remains covered by their full suites.

Publication checkpoints/current-material signer/native BLS Review/Merge are implemented by their own P3/P6/P7 tasks and are not fabricated as P5 proof. P5 exposes the finalized immutable permission they will consume. No user delegation, approval-revocation endpoint, or cancellation of accepted history is added. P5 implementation claims above still require three full independent reports on this source/spec, with no unresolved findings. This evidence alone does not close the task.
