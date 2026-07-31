# CKB sparse-merkle-tree provenance

- Repository: `https://github.com/nervosnetwork/sparse-merkle-tree`
- Release: `v0.6.1`
- Commit: `ad555350c866b2265d87d2d7fbd146fbc918bfe5`
- Imported production files: `h256.rs`, `merge.rs`, `tree.rs`,
  `merkle_proof.rs`, `traits.rs`, and `error.rs`.
- Test-only pristine snapshot: `../sparse-merkle-tree-pristine/src/`.

The pinned commit's Cargo manifest declares MIT but its archive contains no
license file. `LICENSE` carries the standard MIT grant and contributor
attribution retained with this vendored subset.

`UPSTREAM.sha256` pins every pristine imported file. `VENDORED.sha256` pins the
production subset after the allowlisted safety changes. CI verifies both files
and compares the complete production-vs-pristine manifest/source diff
byte-for-byte with `ALLOWLIST.patch`, then rejects panic/unchecked/unsafe
constructs in the production subset. The production checksums are:

| file | vendored SHA-256 |
| --- | --- |
| `h256.rs` | `24a51022e8a934fad540fea5a5838569bdbdba2bec5ecc4774522d8025e051b1` |
| `merge.rs` | `9f8ffb68a36570799b4693594f688e730ae3bc1d25ba7caa5afa664c0605bb0b` |
| `tree.rs` | `c2b1737406de444cffb5f20f9cc8ccdbe8bfc0676963328c357f9fe2abf9076d` |
| `merkle_proof.rs` | `4d3dd8388d1426bd1002c9134b1e5a35fea0fe4906ba58b7e75d9e595d3d354d` |
| `traits.rs` | `b32298e85044c1d25557070653e8aa13cdcd8c041bf4e5630f5b0d867e86ce11` |
| `error.rs` | `e6c39a848ef1c2cabd0900e81c23f7293027a5b78f106594c0d00eeed4a7454a` |

## Allowlisted production diff

No tree, path, ordering, merge, update/delete, proof opcode, or dedup algorithm
is changed. The complete allowed source diff is:

- `error.rs`: add `TreeInvariant(&'static str)` plus its `Display` arm.
- `tree.rs`: replace both non-empty queue `expect` calls with
  `TreeInvariant`; replace the impossible bitmap sibling `unreachable` with
  `TreeInvariant`; replace the stack `debug_assert` and terminal `assert_eq`
  with `CorruptedStack` errors.
- `merkle_proof.rs`: replace the trie-only `unreachable` with
  `CorruptedProof`; replace both stack `debug_assert` sites, every guarded
  `stack.pop().unwrap`, and every guarded `last/last_mut().unwrap` with the
  existing structured proof/stack errors.
- `src/lib.rs` and `Cargo.toml`: minimal std-only crate glue; excluded Blake2,
  C/`smtc`, trie, WASM, CLI, benchmark, and build-script wiring is absent.

Any other source edit requires a new ADR review and updated full-file checksum.
