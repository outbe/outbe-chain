#!/usr/bin/env python3
"""Isolated four-validator process E2E; software SGX fixtures, never hardware evidence."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import shutil
import socket
import subprocess
import time
import urllib.request
import http.server
import threading

ROOT = Path(__file__).resolve().parents[2]
REGISTRY = "0x000000000000000000000000000000000000ee0a"
UPDATE = "0x000000000000000000000000000000000000ee0b"


class Network:
    combined_seal_magic = b"LE2E1"

    def __init__(self, args, scenario):
        self.args, self.scenario = args, scenario
        self.directory = args.output / scenario
        self.directory.mkdir(parents=True, mode=0o700)
        self.network = self.directory / "network"
        self.processes = {}
        self.files = []
        self.log_offsets = {}
        self.checked_nodes = set()
        self.proxies = []
        self.proof_faults = {"window_expired": 0, "ancestor_proofs": 0}
        self.fault_lock = threading.Lock()
        self.offset = args.port_offset
        self.report = {"scenario": scenario, "hardware_sgx": False, "steps": []}
        self.report["cross_epoch"] = args.cross_epoch
        self.report["epoch_length_blocks"] = args.epoch_length_blocks
        self.report["binary_sha256"] = {}
        for name in ("old_node", "new_node", "transition_node", "cli", "enclave", "keygen", "radicle"):
            with getattr(args, name).open("rb") as binary:
                self.report["binary_sha256"][name] = hashlib.file_digest(binary, "sha256").hexdigest()
        if self.report["binary_sha256"]["old_node"] == self.report["binary_sha256"]["new_node"]:
            raise ValueError("old-node and new-node must be different builds")

    def port(self, kind, i=0):
        # Stay below Linux's usual ephemeral port range (32768+).
        return {"rpc": 11000, "consensus": 21000, "p2p": 14000, "discv5": 14100,
                "auth": 15000, "metrics": 16000, "enclave": 17000,
                "radicle": 18000, "status": 19000, "proxy": 24000}[kind] + self.offset + i * (100 if kind == "consensus" else 1)

    def node_dir(self, i):
        return self.network / f"validator-{i}"

    def rpc(self, method, params=(), i=0):
        data = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": list(params)}).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.port('rpc', i)}", data,
                                         {"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=5) as response:
            value = json.load(response)
        if "error" in value:
            raise RuntimeError(value["error"])
        return value["result"]

    def run(self, argv, label, timeout=180):
        with (self.directory / f"{label}.log").open("w") as log:
            result = subprocess.run(list(map(str, argv)), cwd=ROOT, stdout=log,
                                    stderr=subprocess.STDOUT, timeout=timeout)
        if result.returncode:
            raise RuntimeError(f"{label} failed ({result.returncode}); see {label}.log")
        return (self.directory / f"{label}.log").read_text()

    def start(self, name, argv):
        assert name not in self.processes
        log = (self.directory / f"{name}.log").open("a")
        self.log_offsets[name] = log.tell()
        self.files.append(log)
        self.processes[name] = subprocess.Popen(list(map(str, argv)), cwd=ROOT,
            stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)

    def stop(self, name):
        process = self.processes.pop(name, None)
        if process is None:
            return
        forced = False
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                forced = True
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=10)
        if name in self.checked_nodes:
            with (self.directory / f"{name}.log").open() as log:
                log.seek(self.log_offsets[name])
                panic = "panicked at" in log.read()
            check = {"process": name, "exit": process.returncode,
                     "forced_kill": forced, "panic": panic}
            self.report.setdefault("updated_node_shutdowns", []).append(check)
            if forced or process.returncode != 0 or panic:
                raise RuntimeError(f"updated node did not shut down cleanly: {check}")

    def close(self):
        errors = []
        for name in list(self.processes)[::-1]:
            try:
                self.stop(name)
            except Exception as error:
                errors.append(str(error))
        for log in self.files:
            log.close()
        for proxy in self.proxies:
            proxy.shutdown()
            proxy.server_close()
        self.report["proof_faults"] = self.proof_faults
        self.report["shutdown_panic_observations"] = {
            path.name: path.read_text().count("panicked at")
            for path in sorted(self.directory.glob("node-*.log"))
            if "panicked at" in path.read_text()
        }
        if errors:
            raise RuntimeError("; ".join(errors))

    def prepare(self):
        for i in range(4):
            control = self.node_dir(i) / "radicle/node/outbe-control.sock"
            if len(os.fsencode(control)) >= 108:
                raise ValueError(f"test output path exceeds Linux Unix-socket limit: {control}")
        ports = [self.port(kind, i) for i in range(4) for kind in
                 ("rpc", "consensus", "p2p", "discv5", "auth", "metrics", "enclave", "radicle", "status")]
        ports += [self.port("enclave", i+100) for i in range(4)]
        ports += [self.port("consensus", i)+1 for i in range(4)]
        if self.args.inject_proof_faults:
            ports += [self.port("proxy", i) for i in range(4)]
        if len(set(ports)) != len(ports) or any(not 1024 <= p <= 65535 for p in ports):
            raise ValueError("port offset gives overlapping or invalid test ports")
        for port in ports:
            for kind in (socket.SOCK_STREAM, socket.SOCK_DGRAM):
                with socket.socket(socket.AF_INET, kind) as probe:
                    probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                    try:
                        probe.bind(("127.0.0.1", port))
                    except OSError as error:
                        raise RuntimeError(f"test port {port} ({kind.name}) unavailable: {error}") from error
        seed = json.loads((ROOT / "scripts/seed-testnet.json").read_text())
        protocol = seed.setdefault("protocol_constants", {"schemaVersion": 1})
        protocol["governance"] = {"votingWindowBlocks": getattr(self.args, "governance_window_blocks", 20)}
        for contract in seed.get("contracts", []):
            for field in ("code", "state"):
                if contract.get(field):
                    contract[field] = str(ROOT / "scripts/contracts" / contract[field])
        seed_path = self.directory / "seed.json"
        seed_path.write_text(json.dumps(seed))
        self.run(["python3", ROOT / "scripts/prepare_network.py", "--seed", seed_path,
            "--generate-validators", "4", "--validator-hosts", "127.0.0.1", "--output-dir", self.network,
            "--runtime-base-dir", self.network, "--chain-binary", self.args.new_node,
            "--keygen-binary", self.args.keygen, "--runtime-chain-binary", self.args.new_node,
            "--tee-mode", "gramine-direct-dev", "--chain-id", "424242", "--enclave-image", "local-e2e-unused",
            "--runtime-enclave-binary", self.args.enclave, "--epoch-length-blocks", str(self.args.epoch_length_blocks),
            "--dkg-prepare-window-blocks", "30", "--dkg-activation-grace-blocks", "30",
            "--consensus-p2p-base-port", self.port("consensus"), "--consensus-port-stride", "100", "--reth-p2p-base-port", self.port("p2p"),
            "--reth-discv5-base-port", self.port("discv5"), "--rpc-base-port", self.port("rpc"),
            "--authrpc-base-port", self.port("auth"), "--metrics-base-port", self.port("metrics"),
            "--tee-enclave-base-port", self.port("enclave"), "--http-addr", "127.0.0.1",
            "--metrics-addr", "127.0.0.1", "--use-local-defaults"], "prepare", timeout=300)
        self.run([self.args.new_node, "tee", "network-descriptor", "--genesis", self.network / "genesis.json",
                  "--output", self.network / "network-descriptor.bin"], "descriptor")
        self.validators = json.loads((self.network / "validators.json").read_text())
        for i in range(4):
            directory = self.node_dir(i)
            domain = directory / "ocomp/domain-v1"
            domain.mkdir(parents=True, mode=0o700)
            for source, name in [(directory / "evm-key.hex", "ocomp-evm-key.hex"),
                                 (directory / "ocomp-key-v1.hex", "ocomp-key-v1.hex")]:
                shutil.copy2(source, domain / name)
                (domain / name).chmod(0o600)
            shutil.copy2(self.network / "protocol-bundle-v1.ocb1", domain / "protocol-bundle-v1.ocb1")
            if (self.network / "protocol-bundles-v1").exists():
                shutil.copytree(self.network / "protocol-bundles-v1", domain / "protocol-bundles-v1")

        self.genesis_digest = hashlib.sha256((self.network / "genesis.json").read_bytes()).hexdigest()
        self.report["genesis_sha256"] = self.genesis_digest

    def start_enclave(self, i, candidate=False, combined=False):
        name = f"enclave-{'candidate-' if candidate else ''}{i}"
        directory = self.node_dir(i) / ("candidate-tee" if candidate else "tee")
        self.start(name, [self.args.enclave, "--socket", f"127.0.0.1:{self.port('enclave', i + (100 if candidate else 0))}",
            "--tee-dir", directory, "--network-descriptor", self.network / "network-descriptor.bin",
            "--identity-seed", f"{i + (101 if candidate else 1):064x}",
            "--measurement", ("22" if candidate else "11") * 32,
            "--signer", ("bb" if combined else "aa") * 32,
            "--platform", ("dd" if combined else "cc") * 32,
            "--seal-policy", "combined" if combined else "legacy"])

    def start_sidecar(self, i):
        directory = self.node_dir(i)
        for name in ("storage", "node", "cobs"):
            (directory / "radicle" / name).mkdir(mode=0o700, exist_ok=True)
        self.start(f"radicle-{i}", [self.args.radicle, "--home", directory / "radicle",
            "--control-socket", directory / "radicle/node/outbe-control.sock",
            "--listen", f"127.0.0.1:{self.port('radicle', i)}", "--status-listen", f"127.0.0.1:{self.port('status', i)}",
            "--max-validators", "4", "--external-inbound-reserve", "0", "--advertise", f"127.0.0.1:{self.port('radicle', i)}"])
        control = directory / "radicle/node/outbe-control.sock"
        deadline = time.monotonic() + 30
        while not control.is_socket():
            if self.processes[f"radicle-{i}"].poll() is not None:
                raise RuntimeError(f"local sidecar {i} exited before binding its control socket")
            if time.monotonic() >= deadline:
                raise TimeoutError(f"local sidecar {i} did not bind its control socket")
            time.sleep(0.05)

    def start_node(self, i, new=False, candidate=False, validator=True, upstream=None):
        directory = self.node_dir(i)
        key = directory / "reth-p2p-secret.hex"
        key.write_text(key.read_text().strip())
        bootnodes = ",".join((self.network / "reth-bootnodes.txt").read_text().splitlines())
        binary = self.args.new_node if candidate else self.args.transition_node
        argv = [binary if new else self.args.old_node, "node", "--validator",
            "--chain", self.network / "genesis.json", "--datadir", directory / "data",
            "--projection.storage-config", directory / "offchain-storage.toml", "--engine.persistence-threshold", "0",
            "--engine.memory-block-buffer-target", "0", "--http", "--http.addr", "127.0.0.1",
            "--http.port", self.port("rpc", i), "--http.api", "eth,net,web3,outbe", "--disable-discovery",
            "--addr", "127.0.0.1", "--trusted-only",
            "--port", self.port("p2p", i), "--discovery.v5.port", self.port("discv5", i),
            "--bootnodes", bootnodes, "--trusted-peers", bootnodes, "--p2p-secret-key", key,
            "--authrpc.port", self.port("auth", i), "--ipcpath", directory / "data/reth.ipc",
            "--metrics", f"127.0.0.1:{self.port('metrics', i)}", "--log.file.directory", directory / "logs",
            "--consensus.signing-key", directory / "signing-key.hex", "--validator.evm-key", directory / "evm-key.hex",
            "--consensus.listen-addr", f"127.0.0.1:{self.port('consensus', i)}", "--consensus.use-local-defaults",
            "--radicle.control-socket", directory / "radicle/node/outbe-control.sock",
            "--radicle.status-address", f"127.0.0.1:{self.port('status', i)}",
            "--tee-enclave-socket", f"127.0.0.1:{self.port('enclave', i + (100 if candidate else 0))}",
            "--tee-session-mode", "production-node-host", "--tee-bootstrap-timeout-secs", "180"]
        if not validator:
            argv.remove("--validator")
            for flag in ("--consensus.signing-key", "--validator.evm-key",
                         "--radicle.control-socket", "--radicle.status-address"):
                at = argv.index(flag)
                del argv[at:at+2]
        if upstream is not None:
            argv += ["--upstream", self.client_rpc_url(upstream)]
        if new:
            self.checked_nodes.add(f"node-{i}")
        else:
            self.checked_nodes.discard(f"node-{i}")
        self.start(f"node-{i}", argv)

    def wait_height(self, target, label, timeout=240):
        end = time.monotonic() + timeout
        next_progress = time.monotonic() + 30
        heights = []
        while time.monotonic() < end:
            for name, process in self.processes.items():
                if process.poll() is not None:
                    raise RuntimeError(f"{name} exited ({process.returncode}); see {name}.log")
            try:
                blocks = [self.rpc("eth_getBlockByNumber", ["finalized", False], i) for i in range(4)]
                heights = [int(b["number"], 16) for b in blocks]
                if time.monotonic() >= next_progress:
                    print(f"{self.scenario}: {label}: waiting for {target}, heights={heights}", flush=True)
                    next_progress = time.monotonic()+30
                if min(heights) >= target:
                    height = min(heights)
                    hashes = [self.rpc("eth_getBlockByNumber", [hex(height), False], i)["hash"] for i in range(4)]
                    assert len(set(hashes)) == 1, "validators finalized conflicting blocks"
                    self.report["steps"].append({"label": label, "height": height, "hash": hashes[0]})
                    print(f"{self.scenario}: {label}: finalized height {height}", flush=True)
                    return height
            except (OSError, KeyError, TypeError):
                pass
            time.sleep(1)
        raise TimeoutError(f"{label}: no finalized height {target}; last heights={heights}")

    def call(self, signature, args=(), i=0):
        data = subprocess.check_output(["cast", "calldata", signature, *map(str,args)], text=True).strip()
        return self.rpc("eth_call", [{"to": REGISTRY, "data": data}, "finalized"], i)

    def cli(self, args, i=0, label="cli", rpc_i=0):
        # These are disposable, locally generated test keys, never operator keys.
        key = (self.node_dir(i) / "evm-key.hex").read_text().strip()
        return self.run([self.args.cli, "--rpc-url", self.client_rpc_url(rpc_i),
                         "--private-key", key, *args], label)

    def client_rpc_url(self, i):
        return f"http://127.0.0.1:{self.port('proxy' if self.args.inject_proof_faults else 'rpc', i)}"

    def start_proxies(self):
        network = self
        def handler_for(upstream):
            class Handler(http.server.BaseHTTPRequestHandler):
                def log_message(self, *args):
                    pass
                def do_POST(self):
                    request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    method, params = request['method'], request.get('params', [])
                    response = {'jsonrpc': '2.0', 'id': request['id']}
                    try:
                        inject_expiry = False
                        with network.fault_lock:
                            if method == 'eth_getProof' and network.proof_faults['window_expired'] == 0:
                                network.proof_faults['window_expired'] += 1
                                inject_expiry = True
                        if inject_expiry:
                            # Advance the real finalized chain, then make the
                            # first stale request return Reth's precise error.
                            old = int(params[2], 16)
                            until = time.monotonic() + 15
                            while time.monotonic() < until:
                                if int(network.rpc('eth_getBlockByNumber', ['finalized', False], upstream)['number'], 16) > old:
                                    break
                                time.sleep(0.2)
                            current = int(network.rpc('eth_getBlockByNumber', ['finalized', False], upstream)['number'], 16)
                            stale_params = list(params)
                            stale_params[2] = hex(max(1, current - 129))
                            try:
                                network.rpc('eth_getProof', stale_params, upstream)
                            except RuntimeError as error:
                                original = error.args[0]
                                assert 'distance to target block exceeds maximum proof window' in str(original), original
                                response['error'] = original
                            else:
                                raise AssertionError('real stale MPT request unexpectedly succeeded')
                        elif method == 'outbe_getFinalityProof' and int(params[0]) > network.args.epoch_length_blocks + 30 and int(params[0]) % network.args.epoch_length_blocks != 0:
                            height = int(params[0])
                            # Hide the target's direct certificate, retaining a
                            # real signed descendant and the exact parent block.
                            until = time.monotonic() + 15
                            while True:
                                try:
                                    certified = network.rpc('outbe_getFinalization', [height+1], upstream)
                                    block = network.rpc('outbe_getConsensusBlock', [height], upstream)
                                    break
                                except RuntimeError:
                                    if time.monotonic() >= until:
                                        raise
                                    time.sleep(0.2)
                            response['result'] = dict(certified, ancestorBlocksHex=[block])
                            with network.fault_lock:
                                network.proof_faults['ancestor_proofs'] += 1
                        else:
                            response['result'] = network.rpc(method, params, upstream)
                    except Exception as error:
                        response['error'] = {'code': -32000, 'message': str(error)}
                    data = json.dumps(response).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.send_header('Content-Length', str(len(data)))
                    self.end_headers()
                    try:
                        self.wfile.write(data)
                    except BrokenPipeError:
                        pass
            return Handler
        for i in range(4):
            server = http.server.ThreadingHTTPServer(('127.0.0.1', self.port('proxy', i)), handler_for(i))
            server.daemon_threads = True
            threading.Thread(target=server.serve_forever, daemon=True).start()
            self.proxies.append(server)

    def execute(self):
        self.prepare()
        for i in range(4):
            self.start_sidecar(i)
            self.start_enclave(i)
        time.sleep(2)
        for i in range(4):
            self.start_node(i)
        height = self.wait_height(5, "old-node-bootstrap")
        if self.args.inject_proof_faults:
            self.start_proxies()
        permanent = self.call("tributeOfferPublicKey()")
        assert int(permanent,16) != 0
        roots = [hashlib.sha256((self.node_dir(i)/"tee/sealed_root.bin").read_bytes()).hexdigest() for i in range(4)]
        if self.report["binary_sha256"]["old_node"] != self.report["binary_sha256"]["transition_node"]:
            for i in range(4):
                self.stop(f"node-{i}")
                self.start_node(i, new=True)
                height = self.wait_height(height+2, f"node-{i}-binary-update")
        else:
            self.report["steps"].append({"label": "transition-binary-already-installed"})
        assert self.call("tributeOfferPublicKey()") == permanent
        assert roots == [hashlib.sha256((self.node_dir(i)/"tee/sealed_root.bin").read_bytes()).hexdigest() for i in range(4)]
        # A finalized admission must remain provable after the execution head
        # advances. Exercise the node default; do not override its proof window.
        proof_height = height - 2
        for i in range(4):
            opening = self.rpc("eth_getProof", [REGISTRY, ["0x"+"00"*32], hex(proof_height)], i)
            assert opening["accountProof"] and len(opening["storageProof"]) == 1
        self.report["steps"].append({"label": "historical-registry-proof-on-all-nodes", "height": proof_height})
        if self.scenario != "node":
            if self.args.cross_epoch:
                height = self.wait_height(self.args.epoch_length_blocks + 35, "before-upgrade-real-epoch-transition", timeout=900)
                epochs = [self.rpc("outbe_getEpochInfo", i=i) for i in range(4)]
                self.report["epochs_before_upgrade"] = epochs
                self.check_committee_activation(1, self.args.epoch_length_blocks, "committee-before-upgrade")
            self.upgrade_enclaves(height, permanent)
            if self.args.cross_epoch:
                self.wait_height(self.args.epoch_length_blocks * 2 + 35, "after-upgrade-second-epoch-transition", timeout=900)
                self.report["epochs_after_upgrade"] = [self.rpc("outbe_getEpochInfo", i=i) for i in range(4)]
                self.check_committee_activation(2, self.args.epoch_length_blocks * 2, "committee-after-upgrade")
        assert hashlib.sha256((self.network/"genesis.json").read_bytes()).hexdigest() == self.genesis_digest
        self.report["result"] = "passed"
        if self.args.inject_proof_faults and self.scenario == "migration":
            assert self.proof_faults['window_expired'] == 1
            assert self.proof_faults['ancestor_proofs'] > 0

    def check_committee_activation(self, version, height, label):
        statuses = [self.rpc("outbe_consensusStatus", i=i) for i in range(4)]
        for status in statuses:
            assert status["vrfMaterialVersion"] >= version, status
            assert status["lastDkgActivationHeight"] >= height, status
            assert status["hasThresholdShares"], status
        self.report["steps"].append({"label": label, "statuses": statuses})

    def upgrade_enclaves(self, height, permanent):
        deadline = height + 180
        payload = self.directory / "upgrade.json"
        proposal_payload = {"version": getattr(self.args, "protocol_version", "0.2"), "activationHeight": deadline,
                            "info": "local process E2E", "mrenclave": "0x"+"22"*32}
        voting_window = getattr(self.args, "proposal_voting_window_blocks", None)
        if voting_window is not None:
            proposal_payload["votingWindowBlocks"] = voting_window
        payload.write_text(json.dumps(proposal_payload))
        output = self.cli(["vote", "propose", "--target-module", UPDATE, "--payload-file", payload], label="propose")
        receipt = self.wait_transaction(output)
        proposal = "1"  # Every scenario owns a fresh genesis and its first proposal.
        for i in range(4):
            output = self.cli(["vote", "cast", "--proposal-id", proposal, "--yes"], i=i, label=f"vote-{i}")
            self.wait_transaction(output)
            status = self.cli(["vote", "status", "--proposal-id", proposal], label=f"vote-status-{i}")
            if "status=approved" in status:
                break
        vote_deadline = int(re.search(r"deadline=(\d+)", status)[1])
        expected_window = voting_window or getattr(self.args, "governance_window_blocks", 20)
        assert vote_deadline == int(receipt["blockNumber"], 16) + expected_window, status
        self.report["voting_window_blocks"] = expected_window
        self.wait_height(vote_deadline+1, "governance-voting-window-closed")
        status = self.cli(["vote", "status", "--proposal-id", proposal], label="vote-final-status")
        assert "status=approved" in status, status
        self.wait_height(height+1, "governance-approved")
        for i in range(4):
            donor = 3 if i == 0 else 0
            directory = self.node_dir(i)
            binding_before = self.call("validatorEnclaveBinding(address)", [self.validators[i]["address"]], donor)
            self.start_enclave(i, candidate=True, combined=self.scenario == "migration")
            time.sleep(1)
            common = ["--candidate-enclave-socket", f"127.0.0.1:{self.port('enclave',i+100)}",
                      "--node-data-dir", directory / "data", "--reth-p2p-secret-key", directory / "reth-p2p-secret.hex"]
            self.cli(["tee", "upgrade-prepare", *common, "--active-tee-dir", directory / "tee",
                      "--candidate-tee-dir", directory / "candidate-tee"], i, f"prepare-candidate-{i}", donor)
            binding = "0x" + f"{0x100+i:064x}"
            expiry = int(self.rpc("eth_getBlockByNumber",["finalized",False],donor)["timestamp"],16)+7200
            if self.scenario == "migration":
                if i == 0:
                    self.stop(f"enclave-candidate-{i}")
                    seal = directory / "candidate-tee/sealed_root.bin"
                    assert not seal.exists()
                    shutil.copy2(directory / "tee/sealed_root.bin", seal)
                    self.start_enclave(i, candidate=True, combined=True)
                    process = self.processes[f"enclave-candidate-{i}"]
                    assert process.wait(timeout=10) != 0, "incompatible old seal was accepted"
                    self.stop(f"enclave-candidate-{i}")
                    log = (self.directory / f"enclave-candidate-{i}.log").read_text()
                    assert "unseal" in log.lower(), log
                    seal.unlink()  # Only the disposable copy made immediately above.
                    self.report["steps"].append({"label": "incompatible-seal-rejected"})
                    self.start_enclave(i, candidate=True, combined=True)
                    time.sleep(1)
                provision = ["tee", "upgrade-provision", *common, "--genesis", self.network / "genesis.json",
                             "--binding-id", binding, "--valid-until", str(expiry)]
                if i == 0:
                    provision += ["--legacy-direct-dev-source"]
                self.cli(provision, i, f"provision-{i}", donor)
                assert binding_before == self.call("validatorEnclaveBinding(address)", [self.validators[i]["address"]], donor), "prepare replaced active binding"
                assert (directory / "candidate-tee/sealed_root.bin").read_bytes().startswith(self.combined_seal_magic)
                if i == 0:
                    self.stop(f"enclave-candidate-{i}")
                    self.start_enclave(i, candidate=True, combined=True)
                    time.sleep(1)
                    self.cli(provision, i, "provision-restart-retry", donor)
            else:
                self.cli(["tee", "upgrade-copy-root", "--node-data-dir", directory / "data"], i, f"copy-root-{i}", donor)
                assert (directory / "candidate-tee/sealed_root.bin").read_bytes() == (directory / "tee/sealed_root.bin").read_bytes()
                self.stop(f"enclave-candidate-{i}")
                self.start_enclave(i,candidate=True)
                time.sleep(1)
            self.cli(["tee", "upgrade-submit", *common, "--binding-id", binding, "--valid-until", str(expiry)], i, f"submit-{i}", donor)
            self.wait_binding(i, binding, donor)
            self.stop(f"node-{i}")
            self.cli(["tee", "upgrade-finalize", "--node-data-dir", directory / "data"], i, f"finalize-{i}", donor)
            self.stop(f"enclave-{i}")
            target = int(self.rpc("eth_blockNumber",i=donor),16)
            self.start_node(i,new=True,candidate=True,validator=False,upstream=donor)
            # Stop immediately on the DB-backed readiness signal. Additional
            # finalized blocks must not be needed to make a restart safe.
            self.wait_log(f"node-{i}", "local TEE recovery anchor durably persisted; validator restart ready", timeout=240)
            self.report["steps"].append({"label": f"candidate-{i}-durable-restart-ready",
                                         "extra_blocks_waited": 0})
            self.stop(f"node-{i}")
            self.start_node(i,new=True,candidate=True)
            height = self.wait_height(target+5, f"candidate-{i}-validator-resumed")
            assert self.call("tributeOfferPublicKey()",i=donor) == permanent, "permanent network key changed"
        self.wait_height(deadline+3,"past-enclave-retirement",timeout=600)
        missed = subprocess.check_output(["cast","keccak","EnclaveUpgradeMissedV1(uint256,address,uint64,bytes32,uint256)"],text=True).strip()
        assert self.rpc("eth_getLogs",[{"address":REGISTRY,"fromBlock":"0x1","toBlock":"finalized","topics":[missed]}]) == [], "unexpected retirement slash"
        for i in range(4):
            assert int(self.call("isValidatorEnclaveReady(address)",[self.validators[i]["address"]]),16)==1
            version = getattr(self.args, "protocol_version", "0.2")
            major, minor = map(int, version.split("."))
            data = subprocess.check_output(["cast", "calldata", "getActiveVersion()"], text=True).strip()
            actual = self.rpc("eth_call", [{"to": UPDATE, "data": data}, "finalized"], i)
            assert int(actual, 16) == (major << 24) | minor, actual
        self.report["steps"].append({"label":"all-candidates-admitted-no-retirement-slash", "permanent_key":permanent})

    def wait_log(self, name, marker, timeout=60):
        end = time.monotonic()+timeout
        while time.monotonic()<end:
            if self.processes[name].poll() is not None:
                raise RuntimeError(f"{name} exited before {marker}")
            with (self.directory/f"{name}.log").open() as log:
                log.seek(self.log_offsets[name])
                if marker in log.read():
                    return
            time.sleep(0.2)
        raise TimeoutError(f"{name}: missing {marker}")

    def wait_transaction(self, output):
        match = re.search(r"transaction sent: (0x[0-9a-fA-F]{64})",output)
        assert match, output
        end = time.monotonic()+90
        while time.monotonic()<end:
            receipt = self.rpc("eth_getTransactionReceipt",[match[1]])
            if receipt:
                assert int(receipt["status"],16)==1, receipt
                return receipt
            time.sleep(1)
        raise TimeoutError("transaction did not execute")

    def wait_binding(self, i, binding, donor):
        end = time.monotonic()+90
        while time.monotonic()<end:
            value=self.call("validatorEnclaveBinding(address)",[self.validators[i]["address"]],donor)[2:]
            if "0x"+value[3*64:4*64] == binding:
                return
            time.sleep(1)
        raise TimeoutError("candidate binding did not finalize")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("old-node", "new-node", "cli", "keygen", "enclave", "radicle", "output"):
        parser.add_argument("--"+name, type=Path, required=True)
    parser.add_argument("--transition-node", type=Path, help="Host-only update before candidate switch (defaults to new-node)")
    parser.add_argument("--protocol-version", default="0.2", help="Protocol version in the enclave Update proposal")
    parser.add_argument("--proposal-voting-window-blocks", type=int)
    parser.add_argument("--scenario", choices=("node", "enclave", "migration", "all"), default="all")
    parser.add_argument("--port-offset", type=int, default=0)
    parser.add_argument("--epoch-length-blocks", type=int, default=300)
    parser.add_argument("--cross-epoch", action="store_true", help="Run upgrade after the first DKG boundary and continue beyond the second")
    parser.add_argument("--inject-proof-faults", action="store_true", help="Local RPC proxy injects proof-window expiry and missing direct admission certificates")
    args = parser.parse_args()
    if args.epoch_length_blocks < 267:
        parser.error("epoch length must satisfy the default OCOMP retention horizon: 7 * epoch > 1868 blocks")
    args.transition_node = args.transition_node or args.new_node
    for name in ("old_node", "new_node", "transition_node", "cli", "keygen", "enclave", "radicle"):
        path = getattr(args,name).resolve()
        if not path.is_file():
            parser.error(f"missing executable: {path}")
        setattr(args,name,path)
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error("output must be a fresh directory; existing network state is never erased")
    scenarios = ("node","enclave","migration") if args.scenario == "all" else (args.scenario,)
    for scenario in scenarios:
        network = Network(args,scenario)
        try:
            network.execute()
        except BaseException as error:
            network.report.update(result="failed", error=str(error))
            raise
        finally:
            try:
                network.close()
            except BaseException as error:
                network.report.update(result="failed", cleanup_error=str(error))
                raise
            finally:
                (network.directory/"result.json").write_text(json.dumps(network.report,indent=2)+"\n")
        print(f"{scenario}: PASSED (including updated-node shutdown checks)", flush=True)


if __name__ == "__main__":
    main()
