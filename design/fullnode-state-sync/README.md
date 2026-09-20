# Offline snapshots

**Stop outbe-chain, OCOMP and CE → create a signed snapshot of ready native files
→ transfer/place files → optional offline validation → ordinary startup.**

The goal is to let any user start a new node from a snapshot height and state,
without replaying the chain from genesis. The snapshot includes chain and OCOMP
data needed for continuation and independent offline checks. Normal startup
resumes that state and catches up with the network.

- [Operator workflow and responsibilities](operator-workflow.md).
- [Concrete implementation tasks, dependencies, tests and DoD](task-planning/README.md).
- [Native data and identity boundary](data-layout.md).
- [Validation inputs, evidence and incorporated corrections](validation-audit.md).
- [Exact file ownership](task-planning/file-ownership.md).

Baseline: main `e5d545cedc872c0e579fd3535e7638d883d5b1f5`, branch
`feat/offline-snapshot`. Beads owns task status. Plans describe intended work;
source inspection is not executed test evidence.

Only necessary specifications recovered from `/home/ubuntu/123` are retained.
The backup remains unchanged. The coarse R01–R04 exports are superseded by the
concrete tasks; old implementation logs and code are not part of this branch.

Creation requires stopped outbe-chain/OCOMP/CE writers. Choosing a quiet point
outside Lysis computation is recommended, not required. No recipient restore
command or data reconstruction is part of the workflow. No live capture, special shutdown, mandatory validation receipt, original-H
restart checks, new runtime bootstrap lifecycle, transport, vendor changes,
dependency upgrades or Credis changes.

Ordinary ExEx processing and payout scheduling remain unchanged. This feature
preserves their native input data; it does not extend historical payout search
or add new transaction work. The unrelated former task07 is cancelled.
