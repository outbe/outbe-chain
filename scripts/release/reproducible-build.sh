#!/usr/bin/env bash
# Single public entrypoint for the deterministic Linux x86_64 ELF build.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/release/reproducible-build.sh --output DIR [options]

Options:
  --release-tag TAG  Immutable release identity (default: commit-<full-sha>).
  --no-cache         Disable Docker build cache (required for rebuild evidence).
  --help             Show this help.

The source checkout must be clean. DIR must be outside the source checkout and
must not contain prior output. Only Linux x86_64 is supported by recipe v1.
EOF
}

output_dir=""
release_tag=""
no_cache=0
while (($#)); do
  case "$1" in
    --output)
      [[ $# -ge 2 ]] || { echo "--output requires a value" >&2; exit 2; }
      output_dir="$2"
      shift 2
      ;;
    --release-tag)
      [[ $# -ge 2 ]] || { echo "--release-tag requires a value" >&2; exit 2; }
      release_tag="$2"
      shift 2
      ;;
    --no-cache)
      no_cache=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unsupported argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[[ -n "${output_dir}" ]] || { echo "--output is required" >&2; exit 2; }

for command in docker git python3 sha256sum; do
  command -v "${command}" >/dev/null || { echo "required command not found: ${command}" >&2; exit 2; }
done

repo_root="$(git rev-parse --show-toplevel)"
cd "${repo_root}"

if [[ -n "$(git status --porcelain=v1 --untracked-files=all)" ]]; then
  echo "reproducible release builds require a clean source tree" >&2
  git status --short >&2
  exit 2
fi

output_dir="$(python3 - "${output_dir}" "${repo_root}" <<'PY'
import pathlib
import sys

output = pathlib.Path(sys.argv[1]).expanduser().resolve()
root = pathlib.Path(sys.argv[2]).resolve()
try:
    output.relative_to(root)
except ValueError:
    print(output)
else:
    raise SystemExit("output directory must be outside the source checkout")
PY
)"

if [[ -e "${output_dir}" && -n "$(ls -A "${output_dir}")" ]]; then
  echo "output directory must be empty: ${output_dir}" >&2
  exit 2
fi
mkdir -p "${output_dir}"

readonly spec=release/reproducible-elf-build-v1.json
mapfile -t build_values < <(
  python3 - "${spec}" <<'PY'
import json
import sys

spec = json.load(open(sys.argv[1], encoding="utf-8"))
print(spec["builder"]["image"])
print(spec["builder"]["debian_snapshot"])
print(" ".join(spec["builder"]["system_packages"]))
print(" ".join(spec["environment"]["rustflags"]))
print(spec["environment"]["cflags"])
print(spec["environment"]["cxxflags"])
PY
)

builder_image="${build_values[0]}"
debian_snapshot="${build_values[1]}"
system_packages="${build_values[2]}"
rustflags="${build_values[3]}"
cflags="${build_values[4]}"
cxxflags="${build_values[5]}"

source_commit="$(git rev-parse --verify 'HEAD^{commit}')"
source_date_epoch="$(git show -s --format=%ct HEAD)"
source_describe="$(git describe --tags --always HEAD)"
release_tag="${release_tag:-commit-${source_commit}}"

printf 'Reproducible ELF build inputs\n'
printf '  source_commit      = %s\n' "${source_commit}"
printf '  release_tag        = %s\n' "${release_tag}"
printf '  SOURCE_DATE_EPOCH  = %s\n' "${source_date_epoch}"
printf '  source_describe    = %s\n' "${source_describe}"
printf '  target             = x86_64-unknown-linux-gnu\n'
printf '  profile            = release\n'
printf '  builder_image      = %s\n' "${builder_image}"
printf '  debian_snapshot    = %s\n' "${debian_snapshot}"
printf '  output_dir         = %s\n' "${output_dir}"

docker_args=(
  build
  --platform linux/amd64
  --file Dockerfile.reproducible
  --target artifacts
  --build-arg "BUILDER_IMAGE=${builder_image}"
  --build-arg "DEBIAN_SNAPSHOT=${debian_snapshot}"
  --build-arg "SYSTEM_PACKAGES=${system_packages}"
  --build-arg "SOURCE_COMMIT=${source_commit}"
  --build-arg "SOURCE_DATE_EPOCH=${source_date_epoch}"
  --build-arg "SOURCE_DESCRIBE=${source_describe}"
  --build-arg "RELEASE_TAG=${release_tag}"
  --build-arg "REPRODUCIBLE_RUSTFLAGS=${rustflags}"
  --build-arg "REPRODUCIBLE_CFLAGS=${cflags}"
  --build-arg "REPRODUCIBLE_CXXFLAGS=${cxxflags}"
  --output "type=local,dest=${output_dir}"
)
if ((no_cache)); then
  docker_args+=(--no-cache)
fi
docker_args+=(.)

docker "${docker_args[@]}"
(
  cd "${output_dir}"
  sha256sum --check SHA256SUMS
)
printf 'Reproducible ELF candidate written to %s\n' "${output_dir}"
