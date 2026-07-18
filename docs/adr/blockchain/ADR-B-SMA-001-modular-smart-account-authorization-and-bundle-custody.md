# ADR-B-SMA-001: Modular smart accounts preserve bundle custody under every execution route

- **Status:** Proposed; current Solidity implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `contracts/smart-account`, Kernel v4/ERC-4337/ERC-7579 integration
- **Depends on:** ADR-B-CRY-001, ADR-B-EVM-001, ADR-B-EVM-005, ADR-B-CAP-001, ADR-B-DEP-001
- **Related flow:** PFS-003

## Context

The smart-account stack creates Kernel accounts with owner and CCA permissions,
ECDSA validation, execution hooks, a bundle-balance plugin and rolling withdrawal
limits. It protects tokens reserved by the Consume-to-Gain protocol while still
allowing owner free-balance spending and bounded CCA withdrawal. This is execution
and authorization infrastructure shared by Core modules, not Credis business state.

## Decision

`SmartAccountFactory` installs one versioned, ordered module package atomically and
derives a deterministic account address from the complete owner/CCA/token/sender
configuration. The installed account exposes two closed authorities:

- the owner may execute arbitrary calls but every token-spending route is guarded so
  post-execution balance never falls below the plugin's reserved bundle balance;
- a CCA permission may transfer only its configured bundle token, through the
  configured caller/hook path, within its rolling limit, and the successful transfer
  decrements reserved balance exactly once.

Top-up is accepted only from configured bundle senders and reconciles actual ERC-20
balance delta with the recorded reserve. Hooks validate complete execution shapes,
including batch, delegatecall, approval/allowance, permit and module-executor routes;
unknown shapes fail closed for protected tokens.

## Authoritative interfaces

- `SmartAccountFactory.createAccount/getAccountAddress` owns deterministic package
  construction and installation.
- `BundleModulePlugin` owns per-account/per-token reserved balances and token set.
- `BundleSpendProtectorHook` owns the owner free-balance invariant.
- `BundleWithdrawHook` plus `WithdrawalLimitPolicy` own CCA token/limit enforcement.
- `CallerHook`, `ECDSASigner` and `SudoPolicy` participate in caller/signature policy
  but may not bypass the reserve invariant.
- Kernel, EntryPoint and bundler are imported execution/trust seams.

## Invariants

- Recorded reserve is nonnegative and no greater than the account's realizable token
  balance after every successful execution.
- A top-up increases reserve by exactly the tokens actually received; fee-on-transfer
  behavior is either measured or rejected.
- One successful CCA withdrawal decrements reserve and rolling allowance exactly once;
  validation simulation, failed execution and replay do not consume either.
- The owner, CCA, plugin executor, fallback handler and nested/batched calls cannot
  transfer or approve spending of protected balance beyond policy.
- Account address and installed permissions cannot be confused by array ordering,
  duplicate tokens/senders or salt collisions.

## State machine, replay and atomicity

Account creation is `Absent -> Installed(version, configuration)` and is idempotent
for the same derivation. Permission installation/removal and policy upgrades are
explicit states, not implicit module callbacks. Top-up and withdrawal plan the token
effect, execute it and commit accounting within one EVM transaction. ERC-4337 nonce
and EntryPoint validation own UserOperation replay; module accounting must remain
unchanged during simulation and failed execution.

## Determinism and bounds

Factory arrays are deduplicated, canonically ordered or order-committed and bounded.
Hook calldata parsing is length-checked and bounds all nested call recursion. Rolling
windows define boundary timestamp, reset rule, overflow behavior and `validUntil`
encoding. External token calls use checked return semantics and reentrancy protection.

## Security, compatibility and activation

The Kernel/EntryPoint/module versions, implementation bytecode, module addresses and
signature envelope are one compatibility profile. A change to hook ordering, selector
decoding, permission ids or account derivation requires a new profile and migration.
Bundler simulation is untrusted advisory input; on-chain validation is authoritative.

## Production-interface verification evidence

Inspected production entrypoints include factory creation, plugin install/top-up and
decrease dispatch, owner spend pre/post hooks, CCA withdrawal hooks, rolling policy,
caller hook and ECDSA signer. Foundry tests construct the real Kernel and EntryPoint,
which is stronger than isolated mocks, but no evidence ledger proves every Kernel
execution mode or adversarial ERC-20 behavior.

## Consequences

Core protocols can rely on a single reserve invariant instead of coupling to Kernel
internals. Adding a new execution mode or token behavior requires proving it cannot
bypass reserve accounting.

## Rejected alternatives

- Protecting only direct `ERC20.transfer` is rejected because approvals, batches,
  executors and delegatecalls can spend the same balance.
- Decrementing reserve during validation is rejected because simulation is replayed.
- Treating configured amount as received amount is rejected for non-standard tokens.

## Open questions and technical debt

- **Critical:** demonstrate that `BundleSpendProtectorHook` covers approvals,
  `transferFrom`, Kernel batch/nested execution, executor modules, fallback routes and
  delegatecall. A selector-only direct-transfer parser is not a closed interface.
- **Critical:** prove `decreaseBundleBalance` and `dispatchDecreaseBalance` cannot be
  invoked by an unintended installed module/caller and cannot decrement twice.
- Audit pre/post-hook behavior when downstream execution reverts, returns false,
  reenters, changes allowance, or uses a fee-on-transfer/rebasing token.
- Define duplicate/empty token and sender arrays, canonical account salt derivation,
  front-running/idempotent deployment and configuration immutability.
- Model permission lifecycle and recovery: owner/CCA key rotation, module uninstall,
  lost key, frozen bundler, EntryPoint/Kernel upgrade and emergency withdrawal.
- Add invariant tests `actual balance >= total reserved`, conservation across random
  top-up/owner/CCA operations, validation simulation purity and UserOp replay.
- Pin audited Kernel v4 and EntryPoint v0.9 commits and verify production deployment
  bytecode/configuration; README assertions are not release evidence.
- Bound bundle token/sender arrays and calldata traversal to close factory and hook DoS.
