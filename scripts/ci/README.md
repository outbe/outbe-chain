# CI caches

The `clippy` and coverage `test` jobs keep Cargo downloads and build artifacts
on the self-hosted runner instead of uploading `target/` to GitHub. The runner
already bind-mounts its persistent `_work` directory at `/__w` in a job container.
`local_rust_cache.py` stores data in its `_outbe_rust_cache/v1/<repository-id>/`
subdirectory, outside both the checkout and `_temp`. No host provisioning or
additional Docker mounts are required.

Each runner installation has its own work directory. Within it, cache keys
separate workflow, job, Git ref and compiler/build configuration. Automatic reuse
does not cross PR/main refs. A job moving to a
different runner starts cold until that runner has seen the same key. Lockfile
changes reuse the directory; Cargo invalidates the affected dependencies.

After checkout, `target/` is a symlink to the persistent build directory. This
preserves the paths used by trybuild and existing repository scripts. Checkout's
next `git clean -ffdx` removes the link, not its destination. `CARGO_HOME` points
to the persistent Cargo directory, and incremental compilation remains disabled
as it was with `rust-cache`. Test commands and coverage cleanup are unchanged.

## Retention and diagnosis

The setup and final maintenance steps expire caches unused for seven days and
evict the least recently used entries to enforce a default 100 GiB disk budget
**per repository, per runner installation**. Set the repository variable
`CI_RUST_CACHE_MAX_GIB` to adjust it. The active entry is protected during setup;
the final step can evict it if that entry alone exceeds the budget. Builds can
temporarily exceed the cache budget while running. After a cancelled run that
does not reach maintenance, the next setup performs cleanup.

Logs report the selected cache path, warm/cold status, evictions and retained
size. A cache hit never skips checks. To discard all entries, bump the `v1`
namespace in the helper (and remove the old namespace on the idle runner).
To clear one entry, remove the reported directory while that runner is idle.

## zstd and the pinned CI image

`Dockerfile.project-toolchain` installs and checks `zstd` in the CI stage. Until
that image is published and its new digest is pinned, `setup-cache-compression`
installs only the missing compression tool inside the existing job container,
using its configured APT snapshot. It is called before the remaining remote
Rust/Yarn caches and is a no-op when `zstd` is already available. Existing image
digests and Rust/Gramine/DCAP versions remain pinned.

When the image is republished, use the normal `toolchain-image.yml` process to
update consumer digests. The compatibility step remains safe to keep. Switching
from gzip to zstd changes the GitHub cache version, so the first remote run with
zstd may start cold.

Run the local cache lifecycle tests with:

```sh
python3 -m unittest discover -s scripts/ci/tests -v
```
