# Experimental profile for the full private lifecycle PoC

This isolated executable answers feasibility, byte/storage ownership and resource questions. It does not change the production node, earlier review snapshots or historical measurements. The governing requirements remain [R00–R18](../PROTOCOL_TRACE_AND_REQUIREMENTS.md).

The user targets phones with 2 or 4 GB total RAM. The wallet proof limit remains **512,000,000 bytes peak resident memory for a fresh process**, including parameter loading, witness preparation, proving and serialization. Server setup is a separate role with a separate resource measurement. The user explicitly requested implementation and measurement on the **current host**, treating the mobile environment as an assumed target. No Android/iOS/WASM port or physical phone measurement is required for this PoC. Host time/RSS remain labelled as host measurements.

## Measurements and experimental choices

- First complete P_link candidate: Groth16 over BN254 with native Baby-Jubjub Pedersen commitment. Monetary inputs remain bounded integers, not field residues. The external commitment is the same one used for VSS and Nod.
- Circuit/source-list capacities are explicit profiles, not an invented production cap of four. The user selected **32 SU for the primary PoC run** and confirmed there is **no protocol maximum**. Other capacities may be sampled for growth. A finite circuit profile is not a proposed protocol cap; unbounded lists eventually require a streaming/chunked proof design.
- Nominal/source/price inputs are fixed6. Gratis/load/cost are fixed18. Preserve the source quotient/remainder relation and distinguish every newly proposed precision rule.
- Public outputs: admitted metadata, commitments, proofs, S and S_l after closure, public Lysis terms, descriptors and authorized COEN withdrawals. No individual nominal/opening may appear in public artifacts.
- Random fixture values and cryptographic randomness are generated privately. Public reports contain counts, hashes, bytes, timings and pass/fail results. Wallet and committee private files are kept in separate explicitly private directories.
- A runnable scenario must cover full proof verification, admission/availability, at least two committee changes, aggregate opening, offline Fidelity, public coefficient calculation, 256-record worker shards, Nod claim/private payment, account/cohort change, COEN withdrawal and expiry residuals. R15–R18 have explicit scenario/coverage results.
- MPC protocol, source proof compatibility, collateral precision, Intex backing/burn and forfeit disclosure choices are recorded with their actual implemented security and economic scope. A plaintext central simulation cannot pass a private-computation gate.
- Distinguish actual distinct records from repeated-proof/component loops and projected million/billion costs. Report scaling formulas separately from measured data.

## Completion evidence required

1. One-command reproduction and correctness/failure scenarios with saved public result files.
2. An R00–R18 table marking each stage as executed, cryptographically verified, simulated, unsupported or blocked, with reasons. No unimplemented stage can be counted as a full private lifecycle PASS.
3. Cold wallet RSS/time/proof+parameter bytes; verifier throughput; actual serialized Tribute/Nod/state bytes; durable writes, repair/handoff and worker timings.
4. A storage matrix for wallet, each committee member, public network/DA, archival state and recovery packages, including retention/pruning conditions.
5. Mobile viability stated within measurement limits, including missing phone/WASM/thermal evidence where applicable.

