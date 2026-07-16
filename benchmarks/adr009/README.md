# ADR-009 shard-count benchmark report

This directory is the reproducibility contract for measuring shard-count
trade-offs. The checked-in harness compares `K = 1, 2, 4, 8, 16, 32` with real deterministic
entity/key derivation, 256/32-key and 4096/256-key existing/touched shapes,
uniform and single-shard distributions, and insert/update/delete/mixed blocks.
Datasets are selected once against `K = 32` and reused byte-for-byte for every
candidate K: balanced K=32 buckets merge evenly at lower powers of two, while
shard 0 at K=32 remains shard 0 for all lower candidates. Criterion retains a
combined full-path distribution plus separate proof/seal, shard-aggregation,
and finalized-MDBX-apply distributions. Fixture construction is outside every
timed interval, and artifact checksum generation is outside production timing.

ADR-009 keeps the active pre-production value at `K_PROVISIONAL = 16`.
ADR-017 owns the complete-system matrix and the final `K_PRODUCTION` decision.
The checked-in target-host short run below is diagnostic evidence for that
later decision; it does not change the ADR-009 value.

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
- Reviewed shard-count decision and rationale:

## 2026-07-16 Xeon short-run evidence

The accepted validator host ran five independent warm repetitions of only the
`small/uniform/insert` full-path case for every candidate K. Each repetition
used 10 samples, a 0.01-second warm-up, and a 0.01-second measurement window:

```sh
cargo bench -p outbe-compressed-entities --bench adr009 -- \
  'full_path/k=(1|2|4|8|16|32)/small/uniform/insert' \
  --warm-up-time 0.01 --measurement-time 0.01 --sample-size 10
```

Host: Intel Xeon E-2388G (8C/16T), 62 GiB RAM, Samsung NVMe RAID1/ext4,
Linux 6.8, Rust 1.96.0, governor `powersave`, turbo enabled, and SGX available
but not entered by the measured path. All five commands completed in
35.54–35.65 seconds, used 242,884–243,120 KiB peak RSS, and reported
153–155% CPU. The host had unrelated background load, retained verbatim in
`host-profile.txt`.

| K | Mean | Median | p99 | Between-run mean spread | Staged bytes |
|---:|---:|---:|---:|---:|---:|
| 1 | 70.902 ms | 70.910 ms | 71.511 ms | 0.672 ms | 1,135,057 |
| 2 | 70.219 ms | 70.263 ms | 70.701 ms | 0.116 ms | 1,138,896 |
| 4 | 74.355 ms | 74.401 ms | 74.987 ms | 0.245 ms | 1,144,135 |
| 8 | 73.036 ms | 73.087 ms | 74.096 ms | 0.237 ms | 1,147,872 |
| 16 | 81.509 ms | 81.414 ms | 82.083 ms | 0.391 ms | 1,153,678 |
| 32 | 91.712 ms | 91.671 ms | 92.433 ms | 0.444 ms | 1,160,460 |

All 30 case manifests have the same dataset checksum
`0x733b330e30e27a843b14d9221e38e84bd68ef2fda6977e17f316618e9369c737`;
the expected root, batch checksum, and staged size are stable for each K across
all five repetitions.

For this scenario, K=8 is the reviewed preferred trade-off: its mean is
2.135 ms (3.01%) above K=1 and 2.817 ms (4.01%) above the latency-minimizing
K=2, while it exposes eight shards, is 1.318 ms (1.77%) faster than K=4, and
adds only 12,815 staged bytes (1.13%) over K=1. This is not a claim that K=8
has the lowest latency or that it is the final production choice. The full
warm/cold workload matrix and co-located off-chain contention measurement are
postponed to ADR-017.

Reproducible evidence is in
`results/2026-07-16-xeon-e2388g-short/`: the host profile, all 300 raw sample
values, all 30 manifests, aggregate statistics, and SHA-256 inventory. The
unabridged Criterion directories and command logs remain on the benchmark host
under `/home/ubuntu/adr009-benchmark-81fd4bc-SAaqnK`.
