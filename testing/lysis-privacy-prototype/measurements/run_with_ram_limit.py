#!/usr/bin/env python3
"""Run one native proof process; check a decimal-byte RSS budget, including PK load.

On macOS the authoritative final high-water mark comes from getrusage children.
A ps sampler kills an over-budget child early; a spike missed by sampling still
fails the final budget check. Run this wrapper fresh for each proof component.
"""
import argparse
import json
import os
from pathlib import Path
import resource
import subprocess
import sys
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument("--limit", type=int, default=512_000_000)
parser.add_argument("--json", required=True)
parser.add_argument("command", nargs=argparse.REMAINDER)
args = parser.parse_args()
command = args.command[1:] if args.command[:1] == ["--"] else args.command
if not command:
    parser.error("a child command is required after --")
output = Path(args.json)
output.parent.mkdir(parents=True, exist_ok=True)
started = time.monotonic()
child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                         text=True, bufsize=1)

def consume():
    with output.with_suffix(".log").open("w") as log:
        for line in child.stdout:
            log.write(line)
            log.flush()
            print(line, end="", flush=True)

reader = threading.Thread(target=consume)
reader.start()
sampled_peak = 0
killed = False
sampler_available = True
while child.poll() is None:
    snapshot = subprocess.run(["ps", "-o", "rss=", "-p", str(child.pid)],
                              capture_output=True, text=True)
    if snapshot.returncode == 0 and snapshot.stdout.strip():
        sampled_peak = max(sampled_peak, int(snapshot.stdout.strip()) * 1024)
        if args.limit and sampled_peak > args.limit:
            child.terminate()
            killed = True
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
            break
    elif snapshot.stderr.strip():
        sampler_available = False
    time.sleep(0.1)
code = child.wait()
reader.join()
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
peak = usage.ru_maxrss * (1 if sys.platform == "darwin" else 1024)
passed = code == 0 and (not args.limit or peak <= args.limit)
result = {
    "command": command, "exit_code": code, "ram_limit_bytes": args.limit,
    "peak_rss_bytes": peak, "sampled_peak_rss_bytes": sampled_peak,
    "killed_on_sampled_limit": killed, "sampler_available": sampler_available,
    "within_ram_limit": passed, "wall_seconds": time.monotonic() - started,
    "rayon_num_threads": os.environ.get("RAYON_NUM_THREADS"),
    "measurement": "fresh native child process, including PK loading; getrusage high-water mark (includes small ps sampler children); no browser/WASM test",
}
output.write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result), flush=True)
sys.exit(0 if passed else 1)
