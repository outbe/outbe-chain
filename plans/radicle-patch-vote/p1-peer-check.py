#!/usr/bin/env python3
"""Real CLI/remote-helper/Outbe-sidecar replication check; only temporary Ed25519 users."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rad", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--sidecar", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    binaries = {name: getattr(args, name).resolve() for name in ("rad", "helper", "sidecar")}
    assert binaries["helper"].name == "git-remote-rad"
    args.evidence.mkdir(parents=True, exist_ok=True)
    transcript = []
    processes = []
    logs = []
    env = os.environ.copy()
    env.update(NO_COLOR="1", RAD_PASSPHRASE="", GIT_TERMINAL_PROMPT="0")
    env["PATH"] = str(binaries["helper"].parent) + os.pathsep + env["PATH"]
    env.pop("SSH_AUTH_SOCK", None)
    env.pop("RAD_HOME", None)
    env.pop("RAD_SOCKET", None)

    def run(argv, cwd, local_env):
        result = subprocess.run([str(x) for x in argv], cwd=cwd, env=local_env,
                                capture_output=True, text=True, timeout=45)
        transcript.append(dict(argv=[str(x) for x in argv], cwd=str(cwd),
                               exit_code=result.returncode, stdout=result.stdout, stderr=result.stderr))
        assert result.returncode == 0, transcript[-1]
        return result.stdout.strip()

    def control(home, command, **fields):
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(12)
            client.connect(str(home / "node" / "control.sock"))
            client.sendall(json.dumps(dict(command=command, **fields)).encode() + b"\n")
            with client.makefile("rb") as reader:
                response = json.loads(reader.readline())
        transcript.append(dict(control=command, fields=fields, response=response))
        return response

    temp = tempfile.TemporaryDirectory(prefix="ob-p1-", dir="/private/tmp")
    try:
        root = Path(temp.name)
        revision = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
        for name in ("rad", "helper"):
            version = run([binaries[name], "--version"], root, env)
            match = re.search(r" \(([0-9a-f]{7,40})\)$", version)
            assert match and revision.startswith(match[1]), (revision, version)
        peers = []
        for alias in ("alice", "bob"):
            home = root / alias
            peer_env = dict(env, RAD_HOME=str(home), RAD_SOCKET=str(home / "node" / "control.sock"))
            run([binaries["rad"], "auth", "--alias", alias], root, peer_env)
            config = home / "config.json"
            data = json.loads(config.read_text())
            data["node"]["network"] = "outbe"
            data["preferredSeeds"] = []
            config.write_text(json.dumps(data))
            log = (args.evidence / f"{alias}-sidecar.log").open("w")
            logs.append(log)
            command = [binaries["sidecar"], "--home", home, "--control-socket", home / "node" / "control.sock",
                       "--listen", "127.0.0.1:0", "--advertise", "127.0.0.1:8776", "--max-validators", "4",
                       "--external-inbound-reserve", "2", "--status-listen", "127.0.0.1:0"]
            proc = subprocess.Popen([str(x) for x in command], cwd=root, env=peer_env, stdout=log, stderr=log)
            processes.append(proc)
            transcript.append(dict(spawn=[str(x) for x in command]))
            deadline = time.monotonic() + 10
            while not (home / "node" / "control.sock").exists():
                assert proc.poll() is None and time.monotonic() < deadline, "sidecar failed to become ready"
                time.sleep(0.02)
            nid = control(home, "nodeId")
            address = control(home, "listenAddrs")[0]
            node_config = control(home, "config")
            assert node_config["network"] == "outbe"
            assert node_config["limits"]["connection"]["inbound"] == 5
            peers.append((home, peer_env, nid, address))
        alice, alice_env, alice_id, alice_address = peers[0]
        bob, bob_env, _, _ = peers[1]
        source = root / "source"
        run(["git", "init", "--initial-branch=main", source], root, env)
        run(["git", "config", "user.name", "Outbe P1 fixture"], source, env)
        run(["git", "config", "user.email", "p1@example.invalid"], source, env)
        (source / "README.md").write_text("Heartwood stable Outbe replication fixture\n")
        run(["git", "add", "README.md"], source, env)
        run(["git", "-c", "commit.gpgsign=false", "commit", "-m", "Initial fixture"], source, env)
        expected = run(["git", "rev-parse", "HEAD"], source, env)
        run([binaries["rad"], "init", "--name", "p1-fixture", "--description", "Outbe stable replication",
             "--default-branch", "main", "--public", "--no-confirm"], source, alice_env)
        rid = run([binaries["rad"], "inspect"], source, alice_env)
        assert re.fullmatch(r"rad:z[1-9A-HJ-NP-Za-km-z]+", rid), rid
        run([binaries["rad"], "node", "connect", f"{alice_id}@{alice_address}", "--timeout", "5s"], root, bob_env)
        clone = root / "clone"
        run([binaries["rad"], "clone", rid, clone, "--seed", alice_id, "--timeout", "10s"], root, bob_env)
        assert run(["git", "rev-parse", "HEAD"], clone, env) == expected
        assert (clone / "README.md").read_bytes() == (source / "README.md").read_bytes()
        identity = json.loads(run([binaries["rad"], "inspect", "--identity", rid], root, bob_env))
        assert identity["delegates"] == ["did:key:" + alice_id] and identity["threshold"] == 1
        sigrefs = run([binaries["rad"], "inspect", "--sigrefs", rid], root, bob_env)
        assert re.fullmatch(re.escape(alice_id) + r" [0-9a-f]{40} (parent|root)", sigrefs), sigrefs
        remote_refs = run(["git", "ls-remote", "rad"], clone, bob_env)
        assert expected in remote_refs
        started = time.monotonic()
        result = control(bob, "disconnectBounded", nid=alice_id, expectedAddress=alice_address,
                         timeout=dict(secs=2, nanos=0))
        assert result == "disconnected" and time.monotonic() - started < 3, result
        print(json.dumps(dict(result="PASS", chain_revision=revision, repo=rid, commit=expected,
                              binaries={k: dict(path=str(v), sha256=hashlib.sha256(v.read_bytes()).hexdigest())
                                        for k, v in binaries.items()}), indent=2))
    finally:
        for proc in processes:
            if proc.poll() is None:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
        for log in logs:
            log.close()
        (args.evidence / "peer-transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
        temp.cleanup()


if __name__ == "__main__":
    main()
