# Guarded Kernel overlays

These three MIT-licensed files are derived from the pinned Kernel v4 source.
Run `python3 script/sync-guarded-kernel.py` to reproduce them after restoring
Soldeer dependencies, then run `forge fmt src/kernel/guarded`.

Only shared module-mutation and external-delegatecall extension points are added;
imports of unchanged files resolve to the pinned upstream dependency. Root changes
and selector grants also pass through the mutation check. `GuardedKernel` owns the
Outbe policy and UUPS implementation; arbitrary delegatecalls/upgrades permanently
retire bundle eligibility before executing. Kernel's own UserOperation self-dispatch
remains unchanged. No dependency cache edits are required.
