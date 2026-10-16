# Native bootstrap to actual registry checkpoint

Test sourceaf1b58024; production bytes unchanged fromb827c090c. checks.json records
full source/spec/toolchain/env/platform/argv/UTC/exits/log hashes.74 adapter tests,
adapter all-target Clippy-Dwarnings and touched-test fmt PASS. Seven consumer builds
on productionb827c090c remain applicable; this delta changes only the integration test.

The actual actor-generated publication attestations over the full native bootstrap
package now enter RadicleRegistry::apply_publication_batch using HashMapStorageProvider
with real ValidatorSet activation/epoch2/current committee and exact OGD1 allocation.
Two identities yield pending/tally2/no bound RID. The third yields an actual stored
PublicationReceipt,sequence1 and the exact native RID binding. Different3-of4 subsets
in the two independent collectors still produce one package/body/receipt hash.

That actual returned registry receipt replaces the former constructed receipt in the
existing current/historical native proof+policy test. Public receipt observation block
is still fixture-created finality; no real block finalization or genesis/DKG provenance
is claimed. Existing codec,atomicity,current-set and all adapter regressions remain green.

Scope remains intermediate P3/P4: no EVM block assembly/multi-validator consensus,
automatic sidecar signing/collector bridge,normal ref promotion/announcement/restart
or production E2E. No task closed.
