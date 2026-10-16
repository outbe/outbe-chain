#!/usr/bin/env python3
"""Run the P5 checks against frozen committed source and retain exact results."""
import datetime
import hashlib
import json
import os
import pathlib
import platform
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[5]
OUT = pathlib.Path(__file__).resolve().parent
SOURCE = "91644eb78148e7b81cd8e46478dfb0466d321e27"
ENV = {
    "CARGO_TARGET_DIR": "/private/tmp/outbe-radicle-p1-target",
    "CARGO_PROFILE_TEST_DEBUG": "0",
    "CARGO_PROFILE_DEV_DEBUG": "0",
    "CARGO_INCREMENTAL": "0",
    "CARGO_BUILD_JOBS": "4",
}

def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()

assert git("rev-parse", "HEAD") == SOURCE
assert not git("diff", "--name-only"), "tracked source must remain frozen"
spec = ROOT / "plans/radicle-patch-vote/task-specs.json"
task = next(t for t in json.loads(spec.read_text()) if t["key"] == "P5")
(OUT / "spec.json").write_text(json.dumps(task, ensure_ascii=False, indent=2) + "\n")
report = {
    "task": "outbe-chain-pjz.6", "source_sha": SOURCE,
    "base_sha": "8fe62b5dd0e625cd65297479cfa687e3e648ad41",
    "spec_file_sha256": hashlib.sha256(spec.read_bytes()).hexdigest(),
    "task_spec_sha256": hashlib.sha256((OUT / "spec.json").read_bytes()).hexdigest(),
    "platform": platform.platform(), "python": sys.version, "environment": ENV,
    "started_at_utc": utc(), "commands": [],
    "changed_files": git("diff", "--name-only", "8fe62b5dd.." + SOURCE).splitlines(),
}
for tool in (["rustc", "--version"], ["cargo", "--version"], ["git", "--version"], ["forge", "--version"]):
    report.setdefault("toolchain", {})[tool[0]] = subprocess.check_output(tool, cwd=ROOT, text=True).strip()

packages = ["outbe-vote", "outbe-radicleregistry", "outbe-radicle", "outbe-evm"]
package_args = [part for p in packages for part in ("-p", p)]
checks = [
    ("regressions", ["cargo", "test", "--offline", "--locked", *package_args, "-p", "outbe-governance", "-p", "outbe-update", "--lib", "--", "--nocapture"], ROOT),
    ("clippy", ["cargo", "clippy", "--offline", "--locked", *package_args, "--all-targets", "--", "-D", "warnings"], ROOT),
    ("format", ["cargo", "fmt", "--check", *package_args], ROOT),
    ("genesis-layout", ["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_seed_genesis_protocol_constants.py"], ROOT),
    ("abi", ["forge", "inspect", "src/IRadicleRegistry.sol:IRadicleRegistry", "abi", "--json", "--offline"], ROOT / "contracts/precompiles"),
]

for name, argv, cwd in checks:
    log = OUT / (name + ".log")
    entry = {"name": name, "argv": argv, "cwd": str(cwd), "started_at_utc": utc()}
    report["commands"].append(entry)
    with log.open("wb") as stream:
        result = subprocess.run(argv, cwd=cwd, env={**os.environ, **ENV}, stdout=stream, stderr=subprocess.STDOUT)
    entry.update(ended_at_utc=utc(), exit_code=result.returncode, log=log.name,
                 log_sha256=hashlib.sha256(log.read_bytes()).hexdigest())
    if name == "abi" and result.returncode == 0:
        expected = json.loads((ROOT / "contracts/precompiles/abi-export/IRadicleRegistry.json").read_text())
        entry["matches_export"] = json.loads(log.read_text()) == expected
    (OUT / "commands.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(name, result.returncode, flush=True)

assert git("rev-parse", "HEAD") == SOURCE
assert not git("diff", "--name-only"), "source changed during verification"
report["ended_at_utc"] = utc()
report["all_checks_passed"] = all(c["exit_code"] == 0 and c.get("matches_export", True) for c in report["commands"])
(OUT / "commands.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
sys.exit(0 if report["all_checks_passed"] else 1)
