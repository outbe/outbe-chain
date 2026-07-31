---
name: evidence-based-defect-triage
description: Classify suspicious test failures and runtime observations before changing product code. Use when debugging a possible defect, when a verification run finds an anomaly, when a test setup may be impossible or synthetic, or when a focused PASS risks being generalized beyond its assertions.
---

# Evidence-Based Defect Triage

Treat every new anomaly as an `observation`, never as a bug by default. Preserve
the distinction between direct evidence and inference throughout the task.

## Classification

Use exactly one current status:

- `observation`: anomaly seen; cause unknown.
- `hypothesis`: plausible explanation not yet proven.
- `test-defect`: fixture, assertion, timeout, orchestration, or test-model error.
- `environment-failure`: host, sandbox, network, SGX, Docker, or service limitation.
- `confirmed-product-defect`: reachable production behavior violates a proven contract.
- `expected-behavior`: observation matches the contract.
- `not-applicable`: requested state is unreachable or absent from the protocol.
- `unresolved`: available evidence cannot distinguish the remaining explanations.

Do not call `observation`, `hypothesis`, or `unresolved` a bug.

## Triage workflow

1. Record the observation before editing anything:
   - exact SHA, command, environment, timestamps, expected result, actual result;
   - raw logs and state snapshots;
   - the smallest specific postcondition that appears violated.
2. Prove production reachability:
   - identify the public/runtime path that creates the state;
   - reject fixtures that rely on impossible versions, flags, constants, direct
     storage mutation, or an intermediate state hidden by an atomic transition;
   - if no production path exists, classify `test-defect` or `not-applicable`.
3. Establish the expected contract from current code, ADR/flow/spec, or an
   explicit invariant. If sources disagree, classify `unresolved` and report the
   disagreement before changing behavior.
4. Reproduce with the smallest faithful test. Keep environment failures distinct
   from product output. A timeout alone is not a product defect.
5. Exclude competing causes. Check test orchestration, observation boundary,
   finalization, asynchronous projection, version identity, feature flags, and
   environment constraints.
6. Locate the root cause and establish the counterfactual:
   - the faithful regression is red without the product change;
   - the minimal product change makes it green;
   - unrelated assertions and safety contracts remain unchanged.
7. Only now classify `confirmed-product-defect`. Do not modify product code
   earlier unless the user explicitly asks for an exploratory prototype, in
   which case keep it separate from the verdict.
8. State the exact verdict scope. A focused result proves only the assertions it
   executed.

## Finding card

Maintain this record for every material finding:

```text
ID:
Status:
Observed:
Expected:
Production reachability:
Reproduction command:
SHA/environment:
Evidence:
Test setup validity:
Affected invariant/postcondition:
Competing causes excluded:
Root cause:
Counterfactual proof:
Proposed minimal fix:
Regression:
Verdict scope:
```

Before a product edit, every field through `Counterfactual proof` must contain
evidence. Otherwise leave the finding `unresolved` and do not change product
behavior.

## Scope discipline

Never infer any of the following:

- unit PASS implies module correctness;
- focused PASS implies flow correctness;
- mock PASS implies hardware-SGX correctness;
- transaction submission implies execution;
- successful receipt implies finalization or projection;
- no discovered failure implies no defect.

List the observed postconditions behind every PASS or defect claim. Label
source-supported deductions as inference.

## Completion

Finish triage only when every material observation has a status, evidence,
reachability decision, exact verdict scope, and next action. A confirmed product
defect additionally requires a red faithful regression and counterfactual proof.
