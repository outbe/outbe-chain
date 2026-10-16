"""Replay the successful required candidate checks into independent R1 logs."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

base = Path("plans/radicle-patch-vote/evidence/outbe-chain-pjz.2")
out = base / "reviews/R1-artifacts"
lane = sys.argv[1]
records = []
sources = ["root-commands.json", "root-resumed-commands.json"] if lane == "root" else ["heartwood-commands.json"]
for source in sources:
    for command in json.loads((base / "final" / source).read_text()):
        if command["exit_code"] != 0:
            continue
        env = os.environ.copy()
        env.update(command.get("env", {}))
        for key in command.get("unset_env", []):
            env.pop(key, None)
        name = command["name"]
        log = out / (name + ".log")
        started = datetime.datetime.now(datetime.timezone.utc).isoformat()
        with log.open("wb") as output:
            result = subprocess.run(command["argv"], env=env, stdout=output, stderr=subprocess.STDOUT)
        record = dict(name=name, argv=command["argv"], cwd=os.getcwd(), env=command.get("env", {}),
                      unset_env=command.get("unset_env", []), start=started,
                      end=datetime.datetime.now(datetime.timezone.utc).isoformat(), exit_code=result.returncode,
                      log=str(log), sha256=hashlib.sha256(log.read_bytes()).hexdigest())
        records.append(record)
        (out / (lane + "-commands.json")).write_text(json.dumps(records, indent=2) + "\n")
        print(name, result.returncode, flush=True)
        if result.returncode != 0:
            print(log.read_text(errors="replace")[-5000:], flush=True)
            sys.exit(result.returncode)
