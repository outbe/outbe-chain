import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess

base = Path('plans/radicle-patch-vote/evidence/outbe-chain-pjz.1')
out = base / 'reviews/round5/R1-artifacts'
records = []
for c in json.loads((base / 'round5/commands.json').read_text())['commands']:
    env = os.environ.copy()
    overrides = dict(c['environment'])
    if c['name'] == 'design-check':
        overrides['CARGO_TARGET_DIR'] = '/private/tmp/outbe-radicle-p0-r1-round5'
    env.update(overrides)
    log = out / (c['name'] + '.log')
    start = datetime.datetime.now(datetime.timezone.utc).isoformat()
    with log.open('wb') as f:
        result = subprocess.run(c['argv'], env=env, stdout=f, stderr=subprocess.STDOUT)
    records.append(dict(name=c['name'], argv=c['argv'], environment=overrides,
        cwd=os.getcwd(), source_sha='4139bd9ef23c3320b6ac599c65e6123824131e07',
        evidence_sha='6dcea234af6d9018f1661232490a115f5d0cf77f', start=start,
        end=datetime.datetime.now(datetime.timezone.utc).isoformat(), exit_code=result.returncode,
        log=str(log), log_sha256=hashlib.sha256(log.read_bytes()).hexdigest()))
    (out / 'commands.json').write_text(json.dumps(records, indent=2) + '\n')
    print(c['name'], result.returncode, flush=True)
    if result.returncode != 0:
        print(log.read_text(errors='replace')[-5000:], flush=True)
        raise SystemExit(result.returncode)
