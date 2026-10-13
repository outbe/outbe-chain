#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="${repo_root}/release/project-toolchain-v1.json"
install_dir="${1:-${RUNNER_TEMP:-/tmp}/outbe-zk-tools/bin}"

for command in python3 curl sha256sum tar install; do
  command -v "${command}" >/dev/null || {
    echo "required command is unavailable: ${command}" >&2
    exit 2
  }
done

test -f "${manifest}" || {
  echo "project toolchain manifest is unavailable: ${manifest}" >&2
  exit 2
}

work_dir="$(mktemp -d)"
cleanup() {
  rm -rf "${work_dir}"
}
trap cleanup EXIT

mkdir -p "${install_dir}"

python3 - "${manifest}" <<'PY' > "${work_dir}/tools.tsv"
import json
import re
import sys
from pathlib import Path

manifest = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
tools = manifest.get("zk_tools")
if not isinstance(tools, dict):
    raise SystemExit("project toolchain manifest has no zk_tools object")

for name, binary in (("nargo", "nargo"), ("barretenberg", "bb")):
    tool = tools.get(name)
    if not isinstance(tool, dict):
        raise SystemExit(f"project toolchain manifest has no {name} pin")
    version = tool.get("version")
    url = tool.get("archive_url")
    digest = tool.get("archive_sha256")
    if not isinstance(version, str) or not version:
        raise SystemExit(f"invalid {name} version")
    if not isinstance(url, str) or not url.startswith("https://"):
        raise SystemExit(f"invalid {name} HTTPS archive URL")
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise SystemExit(f"invalid {name} archive SHA-256")
    if any(character in value for value in (version, url, digest) for character in "\t\r\n"):
        raise SystemExit(f"invalid control character in {name} pin")
    print(name, binary, version, url, digest, sep="\t")
PY

expected_nargo=""
expected_bb=""
while IFS=$'\t' read -r name binary version url digest; do
  archive="${work_dir}/${name}.tar.gz"
  extract_dir="${work_dir}/${name}"
  mkdir "${extract_dir}"
  curl --fail --location --silent --show-error \
    --proto '=https' --tlsv1.2 --retry 3 \
    --output "${archive}" "${url}"
  printf '%s  %s\n' "${digest}" "${archive}" | sha256sum -c -
  tar -xzf "${archive}" -C "${extract_dir}"
  test -f "${extract_dir}/${binary}" || {
    echo "${name} archive does not contain top-level ${binary}" >&2
    exit 2
  }
  install -m 0755 "${extract_dir}/${binary}" "${install_dir}/${binary}"
  case "${binary}" in
    nargo) expected_nargo="${version}" ;;
    bb) expected_bb="${version}" ;;
  esac
done < "${work_dir}/tools.tsv"

nargo_version="$("${install_dir}/nargo" --version | sed -n 's/^nargo version = //p')"
bb_version="$("${install_dir}/bb" --version | head -n 1)"

test "${nargo_version}" = "${expected_nargo}" || {
  echo "nargo version mismatch: expected ${expected_nargo}, got ${nargo_version}" >&2
  exit 2
}
test "${bb_version}" = "${expected_bb}" || {
  echo "bb version mismatch: expected ${expected_bb}, got ${bb_version}" >&2
  exit 2
}

printf 'Installed nargo %s and bb %s in %s\n' \
  "${nargo_version}" "${bb_version}" "${install_dir}"
