#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="${repo_root}/release/project-toolchain-v1.json"
install_dir="${1:-${RUNNER_TEMP:-/tmp}/outbe-zk-tools/bin}"
work_dir="$(mktemp -d)"
trap 'rm -rf "${work_dir}"' EXIT

mkdir -p "${install_dir}"

for entry in nargo:nargo barretenberg:bb barretenberg_static:libbb-external.a; do
  name="${entry%%:*}"
  binary="${entry##*:}"
  version="$(jq -er --arg name "${name}" '.zk_tools[$name].version' "${manifest}")"
  url="$(jq -er --arg name "${name}" '.zk_tools[$name].archive_url' "${manifest}")"
  digest="$(jq -er --arg name "${name}" '.zk_tools[$name].archive_sha256' "${manifest}")"
  [[ "${url}" == https://* ]]
  [[ "${digest}" =~ ^[0-9a-f]{64}$ ]]

  archive="${work_dir}/${name}.tar.gz"
  extract_dir="${work_dir}/${name}"
  mkdir "${extract_dir}"
  curl --fail --location --silent --show-error \
    --proto '=https' --tlsv1.2 --retry 3 \
    --output "${archive}" "${url}"
  printf '%s  %s\n' "${digest}" "${archive}" | sha256sum -c -
  tar -xzf "${archive}" -C "${extract_dir}"
  if [[ "${name}" == barretenberg_static ]]; then
    test "${version}" = "$(jq -er '.zk_tools.barretenberg.version' "${manifest}")"
    mkdir -p "${install_dir}/../lib/barretenberg"
    install -m 0644 "${extract_dir}/${binary}" "${install_dir}/../lib/barretenberg/${binary}"
    continue
  fi
  install -m 0755 "${extract_dir}/${binary}" "${install_dir}/${binary}"

  case "${binary}" in
    nargo)
      actual="$("${install_dir}/${binary}" --version | sed -n 's/^nargo version = //p')"
      ;;
    bb)
      actual="$("${install_dir}/${binary}" --version | head -n 1)"
      ;;
  esac
  test "${actual}" = "${version}"
done

if [[ -n "${GITHUB_PATH:-}" ]]; then
  printf '%s\n' "${install_dir}" >> "${GITHUB_PATH}"
fi
if [[ -n "${GITHUB_ENV:-}" ]]; then
  printf 'BB_LIB_DIR=%s\nBARRETENBERG_VERSION=%s\n' \
    "$(cd "${install_dir}/../lib/barretenberg" && pwd)" \
    "$(jq -er '.zk_tools.barretenberg_static.version' "${manifest}")" >> "${GITHUB_ENV}"
fi
