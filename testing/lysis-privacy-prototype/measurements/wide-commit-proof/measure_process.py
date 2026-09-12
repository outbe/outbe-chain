#!/usr/bin/env python3
"""One binary stage per fresh process; macOS wait4 peak RSS is bytes.

Wallet cap counts validated PK loading, synthesis/proving and proof serialization.
Setup is separate and may be run with --setup; it is never a wallet measurement.
Sampling is a best-effort early kill; kernel peak RSS decides the final PASS.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import signal
import subprocess
import time

p = argparse.ArgumentParser()
p.add_argument("mode", choices=["synth", "setup", "prove", "verify", "negative"])
p.add_argument("out", type=Path)
p.add_argument("--binary", type=Path, default=Path(__file__).parent / "target/release/outbe-wide-commit-proof")
p.add_argument("--cap", type=int, default=512_000_000)
args = p.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
stdout = args.out / (args.mode + ".stdout")
stderr = args.out / (args.mode + ".stderr")
cap = None if args.mode == "setup" else args.cap
start = time.monotonic()
max_sample = 0
killed = False
with stdout.open("wb") as output, stderr.open("wb") as error:
    child = subprocess.Popen([str(args.binary.resolve()), args.mode, str(args.out.resolve())], stdout=output, stderr=error)
    while True:
        pid, status, usage = os.wait4(child.pid, os.WNOHANG)
        if pid:
            child.returncode = os.waitstatus_to_exitcode(status)
            break
        sample = subprocess.run(["ps", "-o", "rss=", "-p", str(child.pid)], capture_output=True, text=True, check=False)
        try:
            rss = int(sample.stdout.strip()) * 1024
            max_sample = max(max_sample, rss)
            if cap is not None and rss > cap:
                os.kill(child.pid, signal.SIGKILL)
                killed = True
        except (ValueError, ProcessLookupError):
            pass
        time.sleep(0.1)
peak = int(usage.ru_maxrss * (1 if platform.system() == "Darwin" else 1024))
metrics = {
    "stage": args.mode,
    "exit_code": child.returncode,
    "wall_seconds": time.monotonic() - start,
    "peak_rss_bytes_kernel_wait4": peak,
    "peak_rss_bytes_sampled": max_sample,
    "cap_bytes": cap,
    "killed_after_sample_exceeded_cap": killed,
    "cap_pass": None if cap is None else child.returncode == 0 and peak <= cap,
    "setup_is_separate_from_wallet": True,
    "platform": platform.platform(),
}
(args.out / (args.mode + ".resource.json")).write_text(json.dumps(metrics, indent=2) + "\n")
print(json.dumps(metrics, indent=2))
print(stdout.read_text())
if child.returncode:
    print(stderr.read_text())
raise SystemExit(0 if child.returncode == 0 and (cap is None or peak <= cap) else 1)
