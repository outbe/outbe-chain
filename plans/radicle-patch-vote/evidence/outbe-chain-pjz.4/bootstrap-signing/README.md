# Current finalized bootstrap signing — intermediate P3/P4 evidence

Frozen source2604f88e5. checks.json contains full SHA,actual spec hashes,platform,
toolchain,env,argv,cwd,UTC,exits and log hashes.73 adapter lib tests,7 consumers
all-target check,adapter+node all-target Clippy-Dwarnings and8 touched-file fmt pass.
No source change occurred while these checks ran.

manager/{types,snapshot}.rs: sealed FinalizedBootstrapSigningState from one current
canonical finalized state. Existing authority reader validates current ACTIVE/effective
committee. Existing registry governments_descriptor validates bounded OGD1/hash.
Any nonzero network binding,publication sequence/hash or material sequence denies
new bootstrap signatures. Missing/invalid descriptor or finality cannot fall back.

integration/{network,signing}.rs:3 fixed typed bootstrap jobs, bounded existing actor
mailbox/deadline, actor-owned fresh state read/public Output/local provider. Each step
checks descriptor/current context/material/group and authenticated participant mapping.
Only existing provider signs; no Share is copied out.3 typed recovery entry points
verify authenticated contribution frames and recover signatures using a public-only
provider. Neither these SDK methods nor actor commands promote Git refs or finalize.

Behavior:
- snapshot/approval_tests.rs: actual RethSnapshotReader with distinct states by hash;
  same exact state supplies descriptor+committee; no latest/pending fallback. Disabled,
  corrupt descriptor,each accepted-binding field,missing/noncanonical finality reject.
- bootstrap/tests.rs + network/material_tests.rs: actual actor tasks for4 providers,
  all3 steps,two collectors3-of4. Missing Output,missing signer,changed descriptor,
  withdrawn bootstrap permission,material transition and publisher withdrawal deny.
  Frames match independently encoded authenticated messages. Public-only recovery
  yields identical native envelopes for different subsets;q-1/wrong participant deny.
  Full existing native reconstruction/q-witness/policy test remains green.

Limits: mock chain provider and test bootstrap DKG; actor transport uses existing
NoSend/TestReceiver fixture. No live network genesis/DKG provenance claim. New bootstrap
state/requests are not yet exposed through the protected local sidecar protocol;
collector/runtime automation,bootstrap publication attestations,registration/ref CAS,
restart/join and production E2E remain pending. This is not complete P3/P4 acceptance.
