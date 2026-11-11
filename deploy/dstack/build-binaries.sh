#!/usr/bin/env bash
# Reproducible build of the Outbe binaries for the dstack validator image.
#
# The binaries are built in the pinned project toolchain image
# (Dockerfile.project-toolchain) with the same path remapping, locale, time zone
# and SOURCE_DATE_EPOCH rules as the release ELF recipe. The release recipe
# itself builds a fixed artifact list; this script builds the set and the
# feature flags that the dstack image needs.
#
# Usage: build-binaries.sh --output DIR [--check] [--no-cache]
#
#   --output DIR  empty directory outside the source checkout
#   --check       build twice from scratch and compare the results byte for byte
#   --no-cache    do not use the Docker cache for the toolchain image
#
# Environment:
#   JOBS          cargo parallelism and container CPU limit (default: 4)
#   CARGO_CACHE   directory for the cargo registry and git caches
#                 (default: $HOME/outbe-tdx-mvp/cargo-cache)
#
# The source is the working tree: tracked files plus untracked files that are
# not ignored. Its identity is the git tree hash, so two checkouts with the same
# content build the same binaries.
set -euo pipefail

output_dir=""
check=0
no_cache=0
while (($#)); do
    case "$1" in
        --output) output_dir=${2:?--output requires a value}; shift 2 ;;
        --check) check=1; shift ;;
        --no-cache) no_cache=1; shift ;;
        *) echo "unsupported argument: $1" >&2; exit 2 ;;
    esac
done
[[ -n "${output_dir}" ]] || { echo "--output is required" >&2; exit 2; }

JOBS=${JOBS:-4}
CARGO_CACHE=${CARGO_CACHE:-$HOME/outbe-tdx-mvp/cargo-cache}
repo_root=$(git rev-parse --show-toplevel)
cd "${repo_root}"

output_dir=$(mkdir -p "${output_dir}" && cd "${output_dir}" && pwd)
case "${output_dir}/" in
    "${repo_root}"/*) echo "output directory must be outside the source checkout" >&2; exit 2 ;;
esac
[[ -z "$(ls -A "${output_dir}")" ]] || { echo "output directory must be empty: ${output_dir}" >&2; exit 2; }

# Snapshot the working tree into a git tree object without touching the index
# or the branch.
snapshot_index=$(mktemp)
trap 'rm -f "${snapshot_index}"' EXIT
GIT_INDEX_FILE=${snapshot_index} git read-tree HEAD
GIT_INDEX_FILE=${snapshot_index} git add -A
tree=$(GIT_INDEX_FILE=${snapshot_index} git write-tree)
commit=$(git rev-parse HEAD)
source_date_epoch=$(git log -1 --format=%ct HEAD)
describe="${commit:0:12}-tree-${tree:0:12}"

rustflags='--remap-path-prefix=/workspace=/usr/src/outbe-chain --remap-path-prefix=/usr/local/cargo/registry=/cargo/registry --remap-path-prefix=/usr/local/cargo/git=/cargo/git --remap-path-prefix=/usr/local/rustup=/rustup -C link-arg=-Wl,--build-id=sha1'
cflags='-ffile-prefix-map=/workspace=/usr/src/outbe-chain -ffile-prefix-map=/usr/local/cargo/registry=/cargo/registry -ffile-prefix-map=/usr/local/cargo/git=/cargo/git -ffile-prefix-map=/usr/local/rustup=/rustup'

printf 'Reproducible dstack binaries build\n'
printf '  source_commit      = %s\n' "${commit}"
printf '  source_tree        = %s\n' "${tree}"
printf '  SOURCE_DATE_EPOCH  = %s\n' "${source_date_epoch}"
printf '  jobs               = %s\n' "${JOBS}"

toolchain_context=$(mktemp -d -t outbe-dstack-toolchain.XXXXXXXX)
git archive --format=tar "${tree}" | tar -xf - -C "${toolchain_context}"
image="outbe-dstack-toolchain:${tree:0:16}"
docker_build=(build --platform linux/amd64 --file "${toolchain_context}/Dockerfile.project-toolchain"
    --target toolchain --tag "${image}")
((no_cache)) && docker_build+=(--no-cache)
docker "${docker_build[@]}" "${toolchain_context}"
rm -rf "${toolchain_context}"

mkdir -p "${CARGO_CACHE}/registry" "${CARGO_CACHE}/git"

build_once() {
    local out=$1 source
    source=$(mktemp -d -t outbe-dstack-source.XXXXXXXX)
    git archive --format=tar "${tree}" | tar -xf - -C "${source}"
    mkdir -p "${out}"
    docker run --rm --platform linux/amd64 --cpus "${JOBS}" \
        -v "${source}:/workspace" \
        -v "${out}:/out" \
        -v "${CARGO_CACHE}/registry:/usr/local/cargo/registry" \
        -v "${CARGO_CACHE}/git:/usr/local/cargo/git" \
        -w /workspace \
        -e "JOBS=${JOBS}" \
        -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
        -e "SOURCE_DATE_EPOCH=${source_date_epoch}" \
        -e "VERGEN_GIT_SHA=${commit}" \
        -e "VERGEN_GIT_DESCRIBE=${describe}" \
        -e VERGEN_GIT_DIRTY=false \
        -e "RUSTFLAGS=${rustflags}" \
        -e "CFLAGS=${cflags}" -e "CXXFLAGS=${cflags}" \
        -e LANG=C -e LC_ALL=C -e TZ=UTC -e ZERO_AR_DATE=1 \
        "${image}" bash /workspace/deploy/dstack/build-binaries-in-container.sh
    rm -rf "${source}"
    (cd "${out}" && sha256sum --check --quiet SHA256SUMS)
}

if ((check)); then
    build_once "${output_dir}/a"
    build_once "${output_dir}/b"
    if diff -u "${output_dir}/a/SHA256SUMS" "${output_dir}/b/SHA256SUMS"; then
        printf 'reproducibility check passed: %s files identical\n' \
            "$(wc -l < "${output_dir}/a/SHA256SUMS")"
    else
        echo "reproducibility check FAILED: the two builds differ" >&2
        exit 1
    fi
else
    build_once "${output_dir}"
fi
printf 'binaries written to %s\n' "${output_dir}"
