# ADR-009 shard-count benchmark report

This directory is the reproducibility contract for selecting `K_TARGET`. The
checked-in harness compares `K = 1, 2, 4, 8, 16, 32` with real deterministic
entity/key derivation, 256/32-key and 4096/256-key existing/touched shapes,
uniform and single-shard distributions, and insert/update/delete/mixed blocks.
Datasets are selected once against `K = 32` and reused byte-for-byte for every
candidate K: balanced K=32 buckets merge evenly at lower powers of two, while
shard 0 at K=32 remains shard 0 for all lower candidates. Criterion retains a
combined full-path distribution plus separate proof/seal, shard-aggregation,
and finalized-MDBX-apply distributions. Fixture construction is outside every
timed interval, and artifact checksum generation is outside production timing.

No target-host result is checked in yet, and this file deliberately does not
select `K_TARGET`. An Apple developer run is useful only for harness validation.

## Commands

Warm-cache matrix:

```sh
cargo bench -p outbe-compressed-entities --bench adr009
```

Cold-cache samples use a two-process runner. Preparation builds and finalizes
the exact parent, prints a persistent fixture directory, and exits:

```sh
cargo bench -p outbe-compressed-entities --bench adr009 -- \
  cold-prepare 16 large uniform mixed
```

Apply the host's reviewed cache-drop procedure only after `cold-prepare` has
exited, then run exactly one measured path with the printed fixture directory:

```sh
cargo bench -p outbe-compressed-entities --bench adr009 -- \
  cold-run /path/printed/by/cold-prepare cache-drop-reviewed-and-completed
```

The final argument is an explicit operator attestation; the runner cannot
inspect the host page cache. `cold-run` performs no Criterion warm-up or
repetition, includes MDBX environment open in the timed path, advances the
fixture once, and emits one `ADR009_COLD_MANIFEST` JSON line. Re-prepare before
every cold sample. Record the reviewed cache-drop command verbatim; a fresh
handle alone is not a cold OS-cache run.

Capture resource accounting around each filtered warm case on Linux:

```sh
/usr/bin/time -v cargo bench -p outbe-compressed-entities --bench adr009 -- 'k=16/large/uniform/mixed'
```

Retain Criterion's raw `target/criterion` output together with this completed
report. The harness prints one `ADR009_MANIFEST` JSON line per completed case;
capture those lines with the run logs. Each manifest records the dataset and
batch checksums, expected root, canonical staged bytes, changed
shard/branch/leaf counts, and both logical and allocated MDBX bytes before and
after finalization.

## Required target profile

- CPU: Intel Xeon E-2388G, 8 cores / 16 threads, base and boost state recorded
- RAM: 64 GiB
- Storage: local NVMe
- SGX: supported; state whether the measured path entered an enclave
- OS and kernel:
- Filesystem, NVMe model, mount options:
- CPU governor and turbo state:
- Rust toolchain:
- Git revision:
- Background load:
- Cache state and cold-cache procedure:

## Dataset and results

- Dataset generator revision/seed: deterministic counters selected at `K = 32` in `benches/adr009.rs`
- Dataset checksum(s):
- Expected root(s):
- Batch checksum(s):
- Criterion raw-results location/checksum:
- Peak RSS and CPU utilization per case:
- MDBX size before/after per case:
- Latency distribution per case:
- Concentrated-work comparison:
- Uniform/locality benefit over `K = 1`:
- Staged-byte and MDBX growth comparison:
- Reviewed `K_TARGET` decision and rationale:

`K_TARGET` remains open until this matrix is completed and reviewed on the
accepted Xeon/NVMe validator host.
