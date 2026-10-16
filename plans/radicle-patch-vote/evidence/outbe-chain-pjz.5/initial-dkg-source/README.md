# P4 prerequisite: distinguish share storage, genesis DKG and central tooling

Read-only source trace at `02f97948b` (full SHA and source hashes in source-audit.json).
This is not P4 implementation/acceptance or proof of the origin of deployed keys.

| Path | Source-backed behavior |
| --- | --- |
| engine/stack/dkg/startup.rs::obtain_threshold_material | Tries saved local material, then explicitly provisioned CLI material; only then calls run_genesis_dkg. Existing recovery/provisioning must not be confused with fresh key generation. |
| engine/stack/dkg/startup/genesis.rs::run_genesis_dkg | Requires InitialGenesisDkg startup gate, calls run_initial_dkg_durable with P2P transport, previous_output=None and previous_share=None. Keeps its returned local share and public Output, optionally saves that node's state. No central bootstrap helper call in this path. |
| consensus/dkg_actor/actor/ceremony/completion.rs::finalize | Local Player::finalize returns public Output plus this participant's Share. This is not recovery of the group secret. |
| consensus/hybrid/vrf_material.rs::sign_repository/recover_repository | Signs with the provider's local share; recovery consumes partial signatures and returns a signature. The new repository interface does not collect secret shares or reconstruct the scalar. |
| consensus/cli.rs::execute_dkg_bootstrap -> dkg.rs::bootstrap_and_save -> bls.rs::bootstrap_dkg_for_participants | Separate centralized tooling creates all shares in one process and distributes them to per-validator directories. This process has enough secret material to reconstruct the group key. It is not the interactive runtime genesis branch. |
| engine/stack/dkg/startup/manual.rs::load_cli_signer | An existing explicitly provisioned share/polynomial/Output can enter startup after consistency/boundary checks. These checks do not prove that the material originally came from a distributed ceremony. |

The repository signing change must therefore make the narrow verified statement:
its new interface retains local shares and combines signatures only. A claim that
the full owner secret never existed in any operator process also needs evidence
of the actual initial key-generation/provisioning path. P4/P8 must exercise the
interactive initial ceremony and automatic governments bootstrap; current Review
fixtures use centralized test material and are insufficient for that claim.

No consensus startup behavior, CLI provisioning, key files or deployment were
changed or executed in this audit. No decision gate or additional scope was opened.
