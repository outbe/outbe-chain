#!/usr/bin/env python3
"""Run the real native-QVL fixture tests inside a minimal Gramine Direct bundle."""

from __future__ import annotations

import importlib.util
import json
import shutil
import subprocess
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
CONTRACT_PATH = REPO_ROOT / "release/dcap-native-qvl-v1.json"
VERIFIER_PATH = REPO_ROOT / "scripts/release/verify_dcap_native_qvl.py"
TEMPLATE_PATH = (
    REPO_ROOT / "crates/system/tee/tests/gramine/native-qvl.manifest.template"
)


def load_verifier():
    spec = importlib.util.spec_from_file_location(
        "verify_dcap_native_qvl", VERIFIER_PATH
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {VERIFIER_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require_command(name: str) -> str:
    path = shutil.which(name)
    if path is None:
        raise RuntimeError(f"required command is missing: {name}")
    return path


def build_test_binary() -> Path:
    process = subprocess.run(
        [
            "cargo",
            "test",
            "-p",
            "outbe-tee",
            "--features",
            "native-dcap",
            "--test",
            "native_qvl",
            "--offline",
            "--no-run",
            "--message-format=json",
        ],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    executables = []
    for line in process.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = event.get("target", {})
        executable = event.get("executable")
        if (
            event.get("reason") == "compiler-artifact"
            and target.get("name") == "native_qvl"
            and event.get("profile", {}).get("test") is True
            and isinstance(executable, str)
        ):
            executables.append(Path(executable))
    if len(executables) != 1:
        raise RuntimeError(
            f"expected one native_qvl test executable, found {len(executables)}"
        )
    return executables[0]


def render_manifest(entrypoint: Path, qvl_lib_dir: Path, output: Path) -> None:
    subprocess.run(
        [
            require_command("gramine-manifest"),
            f"-Dentrypoint={entrypoint}",
            f"-Dqvl_lib_dir={qvl_lib_dir}",
            str(TEMPLATE_PATH),
            str(output),
        ],
        cwd=REPO_ROOT,
        check=True,
    )


def assert_no_forbidden_verifier_syscalls(trace_path: Path) -> None:
    lines = trace_path.read_text(encoding="utf-8").splitlines()
    try:
        verifier_start = next(
            index for index, line in enumerate(lines) if "running 5 tests" in line
        )
    except StopIteration as error:
        raise RuntimeError("native-QVL syscall trace has no test-start marker") from error

    # Gramine initializes internal AF_UNIX IPC, resolves `localhost` and reads
    # CLOCK_REALTIME before the application entrypoint. Those startup calls do
    # not execute in or influence the verifier. From the public-test marker
    # onward, the trace contains only network, clock and write syscalls: allow
    # test output and CLOCK_MONOTONIC duration accounting, and fail closed on
    # every other traced syscall.
    def allowed(line: str) -> bool:
        if " write(" in line:
            return True
        if "clock_gettime(CLOCK_MONOTONIC" in line:
            return True
        if "clock_gettime64(CLOCK_MONOTONIC" in line:
            return True
        if "socket(AF_UNIX" in line or "socketpair(AF_UNIX" in line:
            return True
        if 'sun_path=@"/gramine/' in line and (
            " bind(" in line or " connect(" in line
        ):
            return True
        return any(
            syscall in line
            for syscall in (" listen(", " accept4(", " shutdown(")
        )

    forbidden = [
        line for line in lines[verifier_start + 1 :] if not allowed(line)
    ]
    if forbidden:
        joined = "\n".join(forbidden[:10])
        raise RuntimeError(f"native-QVL Gramine path used forbidden syscalls:\n{joined}")


def main() -> int:
    verifier = load_verifier()
    contract = verifier.load_manifest(CONTRACT_PATH)
    verifier.verify_manifest(contract, Path("/"), check_packages=True)
    source_binary = build_test_binary()

    gramine_direct = require_command("gramine-direct")
    strace = require_command("strace")
    with tempfile.TemporaryDirectory(prefix="outbe-native-qvl-", dir="/tmp") as temp:
        bundle = Path(temp)
        qvl_lib_dir = bundle / "lib"
        qvl_lib_dir.mkdir()
        for artifact in contract["artifacts"]:
            shutil.copy2(artifact["path"], qvl_lib_dir / artifact["install_name"])

        entrypoint = bundle / "native-qvl-test"
        shutil.copy2(source_binary, entrypoint)
        entrypoint.chmod(0o755)

        application = bundle / "native-qvl"
        manifest = application.with_suffix(".manifest")
        trace = bundle / "forbidden-syscalls.trace"
        render_manifest(entrypoint, qvl_lib_dir, manifest)
        subprocess.run(
            [
                strace,
                "-f",
                "-qq",
                "-e",
                "trace=network,clock_gettime,clock_gettime64,gettimeofday,time,write",
                "-s",
                "256",
                "-o",
                str(trace),
                gramine_direct,
                str(application),
                "--test-threads=1",
            ],
            cwd=REPO_ROOT,
            check=True,
        )
        assert_no_forbidden_verifier_syscalls(trace)

    print("native-QVL Gramine feasibility harness passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
