#!/usr/bin/env bash
# Container-only verification helper for APT closure and Intel DCAP native-QVL
# inside Dockerfile.project-toolchain and build-sgx-bundle-in-container.sh.
set -euo pipefail

export LC_ALL=C

subcommand="${1:-}"
shift || true

case "${subcommand}" in
  verify-apt-inputs)
    pin=""
    apt_root=""
    deb_dir=""
    while (($#)); do
      case "$1" in
        --pin) pin="$2"; shift 2 ;;
        --apt-root) apt_root="$2"; shift 2 ;;
        --deb-dir) deb_dir="$2"; shift 2 ;;
        *) echo "unsupported flag: $1" >&2; exit 2 ;;
      esac
    done
    [[ -n "${pin}" && -n "${apt_root}" && -n "${deb_dir}" ]] || {
      echo "usage: $0 verify-apt-inputs --pin FILE --apt-root DIR --deb-dir DIR" >&2
      exit 2
    }
    python3 - "${pin}" "${apt_root}" "${deb_dir}" <<'PY'
import hashlib, json, pathlib, sys
pin = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
root = pathlib.Path(sys.argv[2]).resolve()
deb_dir = pathlib.Path(sys.argv[3]).resolve()
for src in pin["apt_provenance"]["source_files"]:
    p = root / src["path"].lstrip("/")
    if not p.is_file() or p.is_symlink():
        raise SystemExit(f"missing or non-regular APT provenance input: {src['path']}")
    if hashlib.sha256(p.read_bytes()).hexdigest() != src["sha256"]:
        raise SystemExit(f"APT provenance digest mismatch: {src['path']}")
debs = sorted(deb_dir.glob("*.deb"), key=lambda x: x.name)
ledger = "".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in debs)
closure = pin["apt_provenance"]["deb_closure"]
if len(debs) != closure["count"] or hashlib.sha256(ledger.encode("ascii")).hexdigest() != closure["sha256"]:
    raise SystemExit("APT package closure mismatch")
print("project APT source and package closure: OK")
PY
    ;;
  verify-qvl)
    manifest=""
    root="/"
    install_dir=""
    while (($#)); do
      case "$1" in
        --manifest) manifest="$2"; shift 2 ;;
        --root) root="$2"; shift 2 ;;
        --install-dir) install_dir="$2"; shift 2 ;;
        *) echo "unsupported flag: $1" >&2; exit 2 ;;
      esac
    done
    [[ -n "${manifest}" ]] || {
      echo "usage: $0 verify-qvl --manifest FILE [--root DIR] [--install-dir DIR]" >&2
      exit 2
    }
    python3 - "${manifest}" "${root}" "${install_dir}" <<'PY'
import hashlib, json, pathlib, subprocess, sys
m = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
root = pathlib.Path(sys.argv[2]).resolve()
dest = pathlib.Path(sys.argv[3]) if sys.argv[3] else None
if root == pathlib.Path("/"):
    dcap = m["intel_dcap"]
    pkgs = {
        dcap["runtime_package"]: dcap["runtime_package_version"],
        dcap["development_package"]: dcap["development_package_version"],
        dcap["headers_package"]: dcap["headers_package_version"],
    }
    for item in m["build_inputs"] + m["artifacts"]:
        pkgs[item["package"]] = item["package_version"]
    for pkg, ver in pkgs.items():
        actual = subprocess.check_output(["dpkg-query", "--show", "--showformat=${Version}", pkg], text=True)
        if actual != ver:
            raise SystemExit(f"native-QVL package version mismatch: {pkg}")
for item in m["build_inputs"] + m["artifacts"]:
    p = (root / item["path"].lstrip("/")).resolve()
    raw = p.read_bytes()
    if len(raw) != item["size"] or hashlib.sha256(raw).hexdigest() != item["sha256"]:
        raise SystemExit(f"native-QVL digest/size mismatch: {item['path']}")
if dest is not None:
    dest.mkdir(parents=True, exist_ok=True)
    if any(dest.iterdir()):
        raise SystemExit("native-QVL install destination must be empty")
    for art in m["artifacts"]:
        raw = (root / art["path"].lstrip("/")).resolve().read_bytes()
        target = dest / art["install_name"]
        target.write_bytes(raw)
        target.chmod(0o644)
print("native-QVL artifact contract verified")
PY
    ;;
  *)
    echo "usage: $0 verify-apt-inputs|verify-qvl ..." >&2
    exit 2
    ;;
esac
