#!/usr/bin/env python3
"""Execute the named rights acceptance cases and retain reproducible evidence."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "docs/validation/rights-acceptance-cases.json"
EXCLUDED = {
    "A06", "A07", "A08", "A09", "A10", "A11", "A13", "A19", "A20",
    "A22", "A28", "A29", "A30", "A35", "A40", "A44", "A45",
}


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()


def command(args, cwd, output, label, report):
    """Write complete tool output before reporting an exit status."""
    print(f"[{label}] {' '.join(args)}", flush=True)
    started = time.monotonic()
    path = output / f"{label}.log"
    with path.open("w") as log:
        result = subprocess.run(args, cwd=cwd, stdout=log, stderr=subprocess.STDOUT)
    entry = {
        "label": label, "command": args, "cwd": str(cwd),
        "exit_code": result.returncode,
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "log": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }
    report["commands"].append(entry)
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(f"[{label}] exit={result.returncode}; log={path}", flush=True)
    if result.returncode:
        raise RuntimeError(f"{label} failed; inspect {path}")
    return path


def validate_manifest(manifest):
    cases = manifest["rust_cases"]
    keys = [(c["package"], c["binary"], c["test"]) for c in cases]
    if not cases or len(keys) != len(set(keys)):
        raise ValueError("Rust cases must be nonempty and unique by package/binary/test")
    for case in cases + manifest["forge_cases"]:
        if not (case.get("acceptance") or case.get("requirements")) or EXCLUDED.intersection(case.get("acceptance", [])):
            raise ValueError(f"Missing or excluded acceptance IDs: {case}")
    for case in cases:
        for field in ("package", "binary", "test"):
            if not re.fullmatch(r"[A-Za-z0-9_:.\-/]+", case[field]):
                raise ValueError(f"Unsupported filter token: {case[field]}")


def rust(manifest, output, report, refresh):
    cases = manifest["rust_cases"]
    packages = sorted({c["package"] for c in cases})
    package_args = [arg for package in packages for arg in ("-p", package)]
    if refresh:
        command(["cargo", "clean", *package_args], ROOT, output, "rust-refresh", report)
    feature_args = []
    if manifest.get("rust_features"):
        feature_args = ["--features", ",".join(manifest["rust_features"])]
    args = ["cargo", "nextest", "list", "--locked", *package_args,
            *feature_args, "--message-format", "json"]
    # stdout is JSON; compiler diagnostics must remain separate.
    print("[rust-inventory] checking every named compiled test", flush=True)
    started = time.monotonic()
    path = output / "rust-inventory.json"
    diagnostics = output / "rust-inventory.stderr.log"
    with path.open("w") as stdout, diagnostics.open("w") as stderr:
        result = subprocess.run(args, cwd=ROOT, stdout=stdout, stderr=stderr)
    report["commands"].append({
        "label": "rust-inventory", "command": args, "cwd": str(ROOT),
        "exit_code": result.returncode,
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "log": str(path), "diagnostics": str(diagnostics),
    })
    if result.returncode:
        raise RuntimeError(f"Rust inventory failed; inspect {diagnostics}")
    inventory = json.loads(path.read_text())
    available = {}
    for suite in inventory["rust-suites"].values():
        for name, detail in suite["testcases"].items():
            key = (suite["package-name"], suite["binary-id"], name)
            available[key] = detail
    for case in cases:
        key = (case["package"], case["binary"], case["test"])
        if key not in available or available[key].get("ignored", False):
            raise RuntimeError(f"Mapped case missing or ignored: {key}")
    report["rust_cases_found"] = len(cases)
    selection = " | ".join(
        f'(binary_id(={c["binary"]}) & test(={c["test"]}))'
        for c in cases
    )
    command(["cargo", "nextest", "run", "--locked", *package_args, *feature_args,
             "--no-tests", "fail", "-E", selection],
            ROOT, output, "rust-acceptance", report)


def solidity(manifest, output, report):
    names = sorted({c["test"] for c in manifest["forge_cases"]})
    if not names:
        raise ValueError("No Forge acceptance cases")
    # Forge matches complete signatures (including parentheses), not bare names.
    pattern = "^(" + "|".join(re.escape(n) for n in names) + r")(\(|$)"
    path = command(["forge", "test", "--match-test", pattern, "--json"],
                   ROOT / "contracts/intex", output, "forge-acceptance", report)
    suites = json.loads(path.read_text())
    observed = {}
    for suite_name, suite in suites.items():
        for name, detail in suite["test_results"].items():
            observed[(suite_name, name.split("(", 1)[0])] = detail["status"]
    for case in manifest["forge_cases"]:
        matching = [status for (suite, name), status in observed.items()
                    if name == case["test"] and suite.endswith(":" + case["contract"])]
        if matching != ["Success"]:
            raise RuntimeError(f"Forge case missing or unsuccessful: {case}; {matching}")
    report["forge_cases_passed"] = len(manifest["forge_cases"])


def client(output, report):
    cwd = ROOT / "mcp"
    command(["npm", "test"], cwd, output, "client-tests", report)
    command(["npx", "tsc", "--noEmit"], cwd, output, "client-types", report)


def fixtures(output, report):
    """Rebuild the real ERC-1155 used by the transfer settlement EVM test."""
    cwd = ROOT / "contracts/intex"
    command(["forge", "build", "--skip", "test", "--skip", "script"],
            cwd, output, "transfer-fixture-build", report)
    fixture_dir = ROOT / "crates/blockchain/evm/tests/fixtures"
    for contract in ("IntexNFT1155", "IntexMetadata"):
        artifact = json.loads((cwd / f"out/{contract}.sol/{contract}.json").read_text())
        deployed = artifact["deployedBytecode"]["object"].removeprefix("0x")
        references = artifact["deployedBytecode"].get("linkReferences", {})
        for libraries in references.values():
            for name, locations in libraries.items():
                if name != "IntexMetadata":
                    raise RuntimeError(f"Unexpected fixture library: {name}")
                for location in locations:
                    if location["length"] != 20:
                        raise RuntimeError("Unexpected library address width")
                    start = location["start"] * 2
                    deployed = deployed[:start] + "4c" * 20 + deployed[start + 40:]
        expected = (fixture_dir / f"{contract}.hex").read_text().strip().removeprefix("0x")
        if deployed.lower() != expected.lower():
            raise RuntimeError(f"{contract}.hex differs from rebuilt Solidity runtime")
        report.setdefault("transfer_fixtures", {})[contract] = hashlib.sha256(
            bytes.fromhex(deployed)).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--phase", choices=("all", "rust", "solidity", "client", "fixtures"),
                        default="all")
    parser.add_argument("--refresh", action="store_true",
                        help="Remove only the mapped workspace packages' cached Rust artifacts")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    manifest = json.loads(args.manifest.read_text())
    validate_manifest(manifest)
    report = {
        "commit": git("rev-parse", "HEAD"), "working_tree": git("status", "--short"),
        "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
        "phase": args.phase, "commands": [], "passed": False,
        "environment": {key: os.environ.get(key) for key in
                        ("CARGO_TARGET_DIR", "CARGO_INCREMENTAL", "SOURCE_DATE_EPOCH")},
    }
    try:
        if args.phase in ("all", "rust"):
            rust(manifest, output, report, args.refresh)
        if args.phase in ("all", "fixtures"):
            fixtures(output, report)
        if args.phase in ("all", "solidity"):
            solidity(manifest, output, report)
        if args.phase in ("all", "client"):
            client(output, report)
        report["passed"] = True
    except (RuntimeError, ValueError, KeyError, json.JSONDecodeError) as error:
        report["error"] = str(error)
        print(str(error), file=sys.stderr)
    finally:
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
